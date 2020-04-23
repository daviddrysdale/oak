//
// Copyright 2020 The Project Oak Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//

use crate::{
    message::Message,
    runtime::{DotIdentifier, HtmlPath},
};
use itertools::Itertools;
use log::{debug, error, warn};
use oak_abi::OakStatus;
use rand::RngCore;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt::Write,
    sync::{
        atomic::{AtomicU64, Ordering::SeqCst},
        Arc, Mutex, RwLock, Weak,
    },
    thread::{Thread, ThreadId},
};

type Messages = VecDeque<Message>;

/// We use a `HashMap` keyed by `ThreadId` to prevent build up of stale `Weak<Thread>`s.
///
/// That is: If a thread waiting/blocked on a channel is woken by a different channel, its
/// `Weak<Thread>` will remain in the first channel's waiting_thread member. If a thread keeps
/// waiting on this first channel, and keeps being woken by other channels, it will keep re-adding
/// itself. We use a `HashMap` and insert at the current `ThreadId` so that we replace any stale
/// `Weak<Thread>`s which will have gone out of scope. (`wait_on_channels` drops the underlying arc
/// as soon as it is resumed.)
type WaitingThreads = Mutex<HashMap<ThreadId, Weak<Thread>>>;

/// The internal implementation of a channel representation backed by a `VecDeque<Message>`.
/// Readers and writers to this channel must increment the reader/writer count. This is implemented
/// for `ChannelWriter`/`ChannelReader`, which will increment/decrement readers/writers when
/// cloning/dropping.
pub struct Channel {
    pub messages: RwLock<Messages>,

    pub writer_count: AtomicU64,
    pub reader_count: AtomicU64,

    /// A HashMap of `ThreadId`s to `Weak<Thread>`s. This allows threads to insert a weak reference
    /// to themselves to be woken when a new message is available. Weak references are used so that
    /// if the thread is woken by a different channel, it can deallocate the underlying `Arc`
    /// instead of removing itself from all the `Channel`s it subscribed to.
    /// Threads can be woken up spuriously without issue.
    pub waiting_threads: WaitingThreads,

    /// The Label associated with this channel.
    ///
    /// This is set at channel creation time and does not change after that.
    ///
    /// See https://github.com/project-oak/oak/blob/master/docs/concepts.md#labels
    pub label: oak_abi::label::Label,
}

impl std::fmt::Debug for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Channel {{ #msgs={}, #readers={}, #writers={}, label={:?} }}",
            self.messages.read().unwrap().len(),
            self.reader_count.load(SeqCst),
            self.writer_count.load(SeqCst),
            self.label,
        )
    }
}

/// An identifier for one half of a [`Channel`]. Each [`ChannelHalfId`] has an implicit direction
/// such that it is only possible to read or write to a [`ChannelHalfId`] (exclusive or).
#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug)]
pub struct ChannelHalfId(u64);

impl DotIdentifier for ChannelHalfId {
    fn dot_id(&self) -> String {
        format!("h{}", self.0)
    }
}

impl HtmlPath for ChannelHalfId {
    fn html_path(&self) -> String {
        format!("/half/{}", self.0)
    }
}

/// The direction of a [`ChannelHalfId`] can be discovered by querying the associated
/// [`Runtime`] with [`channel_half_get_direction`].
///
/// [`Runtime`]: crate::runtime::Runtime
/// [`channel_half_get_direction`]: crate::runtime::Runtime::channel_half_get_direction
#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug)]
pub enum ChannelHalfDirection {
    Read,
    Write,
}

/// An internal identifier to track a [`Channel`]. This is implementation specific and should not
/// be visible outside the internals of [`Runtime`].
///
/// [`Runtime`]: crate::runtime::Runtime
type ChannelId = u64;

impl DotIdentifier for ChannelId {
    fn dot_id(&self) -> String {
        format!("channel{}", self)
    }
}

impl HtmlPath for ChannelId {
    fn html_path(&self) -> String {
        format!("/channel/{}", self)
    }
}

/// Ownership and mapping of [`Channel`]s to [`ChannelHalfId`]s.
pub struct ChannelMapping {
    pub channels: RwLock<HashMap<ChannelId, Channel>>,

    pub next_channel_id: AtomicU64,

    pub readers: RwLock<HashMap<ChannelHalfId, ChannelId>>,
    pub writers: RwLock<HashMap<ChannelHalfId, ChannelId>>,
}

impl Channel {
    /// Create a new channel with the assumption there is currently one active reader and one active
    /// writer.
    pub fn new(label: &oak_abi::label::Label) -> Channel {
        Channel {
            messages: RwLock::new(Messages::new()),
            writer_count: AtomicU64::new(1),
            reader_count: AtomicU64::new(1),
            waiting_threads: Mutex::new(HashMap::new()),
            label: label.clone(),
        }
    }

    /// Determine whether there are any readers of the channel.
    pub fn has_readers(&self) -> bool {
        self.reader_count.load(SeqCst) > 0
    }

    /// Determine whether there are any writers of the channel.
    pub fn has_writers(&self) -> bool {
        self.writer_count.load(SeqCst) > 0
    }

    /// Determine whether there are any users of the channel.
    pub fn has_users(&self) -> bool {
        self.has_readers() || self.has_writers()
    }

    /// Insert the given `thread` reference into `thread_id` slot of the HashMap of waiting
    /// channels attached to an underlying channel. This allows the channel to wake up any waiting
    /// channels by calling `thread::unpark` on all the threads it knows about.
    pub fn add_waiter(&self, thread_id: ThreadId, thread: &Arc<Thread>) {
        self.waiting_threads
            .lock()
            .unwrap()
            .insert(thread_id, Arc::downgrade(thread));
    }

    /// Decrement the [`Channel`] writer counter.
    pub fn dec_writer_count(&self) {
        if self.writer_count.fetch_sub(1, SeqCst) == 0 {
            panic!("remove_reader: Writer count was already 0, something is very wrong!")
        }
    }

    /// Decrement the [`Channel`] reader counter.
    pub fn dec_reader_count(&self) {
        if self.reader_count.fetch_sub(1, SeqCst) == 0 {
            panic!("remove_reader: Reader count was already 0, something is very wrong!")
        }
    }

    /// Increment the [`Channel`] writer counter.
    pub fn inc_writer_count(&self) -> u64 {
        self.writer_count.fetch_add(1, SeqCst)
    }

    /// Increment the [`Channel`] reader counter.
    pub fn inc_reader_count(&self) -> u64 {
        self.reader_count.fetch_add(1, SeqCst)
    }

    // Build an HTML page describing a specific `Channel`.
    fn html(&self) -> String {
        let mut s = String::new();
        write!(
            &mut s,
            "Channel {{ reader_count={}, writer_count={}, label={:?} }}<br/>",
            self.reader_count.load(SeqCst),
            self.writer_count.load(SeqCst),
            self.label
        )
        .unwrap();
        let messages = self.messages.read().unwrap();
        write!(&mut s, r###"Messages: (count = {}):<ul>"###, messages.len()).unwrap();
        for (i, message) in messages.iter().enumerate() {
            write!(
                &mut s,
                "  <li>message[{}]: data.len()={}, halves=[",
                i,
                message.data.len()
            )
            .unwrap();
            for (j, half) in message.channels.iter().enumerate() {
                if j > 0 {
                    write!(&mut s, ", ").unwrap();
                }
                write!(
                    &mut s,
                    r###"<a href="{}">{:?}</a>"###,
                    half.html_path(),
                    half
                )
                .unwrap();
            }
            write!(&mut s, "]").unwrap();
        }

        s
    }
}

impl ChannelMapping {
    /// Create a new empty [`ChannelMapping`].
    pub fn new() -> ChannelMapping {
        ChannelMapping {
            channels: RwLock::new(HashMap::new()),

            next_channel_id: AtomicU64::new(0),

            readers: RwLock::new(HashMap::new()),
            writers: RwLock::new(HashMap::new()),
        }
    }

    /// Creates a new [`Channel`] and returns a `(writer handle, reader handle)` pair.
    pub fn new_channel(&self, label: &oak_abi::label::Label) -> (ChannelHalfId, ChannelHalfId) {
        let channel_id = self.next_channel_id.fetch_add(1, SeqCst);
        self.channels
            .write()
            .unwrap()
            .insert(channel_id, Channel::new(label));
        (self.new_writer(channel_id), self.new_reader(channel_id))
    }

    /// Get a new free random [`ChannelHalfId`] that is not used by any readers or writers.
    fn new_half_id(&self) -> ChannelHalfId {
        loop {
            let handle = ChannelHalfId(rand::thread_rng().next_u64());

            let exists_reader = self.readers.read().unwrap().get(&handle).is_some();
            let exists_writer = self.writers.read().unwrap().get(&handle).is_some();

            if exists_reader || exists_writer {
                continue;
            }

            return handle;
        }
    }

    /// Create a new writer channel half ID.
    fn new_writer(&self, channel_id: ChannelId) -> ChannelHalfId {
        let half_id = self.new_half_id();
        self.writers.write().unwrap().insert(half_id, channel_id);
        half_id
    }

    /// Create a new reader channel half ID.
    fn new_reader(&self, channel_id: ChannelId) -> ChannelHalfId {
        let half_id = self.new_half_id();
        self.readers.write().unwrap().insert(half_id, channel_id);
        half_id
    }

    /// Attempt to retrieve the [`ChannelId`] associated with a reader [`ChannelHalfId`].
    fn get_reader_channel(&self, half_id: ChannelHalfId) -> Result<ChannelId, OakStatus> {
        self.readers
            .read()
            .unwrap()
            .get(&half_id)
            .map_or(Err(OakStatus::ErrBadHandle), |id| Ok(*id))
    }

    /// Attempt to retrieve the [`ChannelId`] associated with a writer [`ChannelHalfId`].
    fn get_writer_channel(&self, half_id: ChannelHalfId) -> Result<ChannelId, OakStatus> {
        self.writers
            .read()
            .unwrap()
            .get(&half_id)
            .map_or(Err(OakStatus::ErrBadHandle), |id| Ok(*id))
    }

    /// Perform an operation on a [`Channel`] associated with a [`ChannelId`].
    /// The channels read lock is held while the operation is performed.
    fn with_channel<U, F: FnOnce(&Channel) -> Result<U, OakStatus>>(
        &self,
        channel_id: ChannelId,
        f: F,
    ) -> Result<U, OakStatus> {
        let channels = self.channels.read().unwrap();
        let channel = channels.get(&channel_id).ok_or(OakStatus::ErrBadHandle)?;
        f(channel)
    }

    /// Perform an operation on a [`Channel`] associated with a reader [`ChannelHalfId`].
    /// The channels read lock is held while the operation is performed.
    pub fn with_reader_channel<U, F: FnOnce(&Channel) -> Result<U, OakStatus>>(
        &self,
        half_id: ChannelHalfId,
        f: F,
    ) -> Result<U, OakStatus> {
        self.with_channel(self.get_reader_channel(half_id)?, f)
    }

    /// Perform an operation on a [`Channel`] associated with a writer [`ChannelHalfId`].
    /// The channels read lock is held while the operation is performed.
    pub fn with_writer_channel<U, F: FnOnce(&Channel) -> Result<U, OakStatus>>(
        &self,
        half_id: ChannelHalfId,
        f: F,
    ) -> Result<U, OakStatus> {
        self.with_channel(self.get_writer_channel(half_id)?, f)
    }

    /// Deallocate a [`ChannelHalfId`] so it is no longer usable in operations,
    /// and the underlying [`Channel`] may become orphaned.
    pub fn remove_half_id(&self, half_id: ChannelHalfId) -> Result<(), OakStatus> {
        if let Ok(channel_id) = self.get_writer_channel(half_id) {
            {
                let mut channels = self.channels.write().unwrap();
                let channel = channels
                    .get(&channel_id)
                    .expect("remove_half_id: ChannelHalfId is invalid!");
                channel.dec_writer_count();
                if !channel.has_users() {
                    channels.remove(&channel_id);
                    debug!("remove_half_id: deallocating channel {:?}", channel_id);
                }
            }

            self.writers.write().unwrap().remove(&half_id);
        }

        if let Ok(channel_id) = self.get_reader_channel(half_id) {
            {
                let mut channels = self.channels.write().unwrap();
                let channel = channels
                    .get(&channel_id)
                    .expect("remove_half_id: ChannelHalfId is invalid!");
                channel.dec_reader_count();
                if !channel.has_users() {
                    channels.remove(&channel_id);
                    debug!("remove_half_id: deallocating channel {:?}", channel_id);
                }
            }

            self.readers.write().unwrap().remove(&half_id);
        }

        Ok(())
    }

    /// Duplicate a [`ChannelHalfId`]. This new identifier and when it is closed
    /// will be tracked separately from the first [`ChannelHalfId`]. For
    /// instance this is used by the [`Runtime`] to encode [`Channel`]s in
    /// messages.
    ///
    /// [`Runtime`]: crate::runtime::Runtime
    pub fn duplicate_half_id(&self, half_id: ChannelHalfId) -> Result<ChannelHalfId, OakStatus> {
        if let Ok(channel_id) = self.get_writer_channel(half_id) {
            self.with_channel(channel_id, |channel| Ok(channel.inc_writer_count()))?;

            return Ok(self.new_writer(channel_id));
        }

        if let Ok(channel_id) = self.get_reader_channel(half_id) {
            self.with_channel(channel_id, |channel| Ok(channel.inc_reader_count()))?;

            return Ok(self.new_reader(channel_id));
        }

        Err(OakStatus::ErrBadHandle)
    }

    /// Determine the direction of a [`ChannelHalfId`].
    pub fn get_direction(&self, half_id: ChannelHalfId) -> Result<ChannelHalfDirection, OakStatus> {
        {
            if self.readers.read().unwrap().contains_key(&half_id) {
                return Ok(ChannelHalfDirection::Read);
            }
        }
        {
            if self.writers.read().unwrap().contains_key(&half_id) {
                return Ok(ChannelHalfDirection::Write);
            }
        }
        Err(OakStatus::ErrBadHandle)
    }

    /// Build a Dot nodes stanza for the `ChannelMapping`.
    #[cfg(feature = "oak_debug")]
    pub fn graph_nodes(&self) -> String {
        let mut s = String::new();
        writeln!(&mut s, "  {{").unwrap();
        writeln!(
            &mut s,
            "    node [shape=hexagon style=filled fillcolor=yellow]"
        )
        .unwrap();

        {
            for half_id in self.writers.read().unwrap().keys() {
                writeln!(
                    &mut s,
                    r###"    {} [URL="{}"]"###,
                    half_id.dot_id(),
                    half_id.html_path()
                )
                .unwrap();
            }
        }
        {
            for half_id in self.readers.read().unwrap().keys() {
                writeln!(
                    &mut s,
                    r###"    {} [URL="{}"]"###,
                    half_id.dot_id(),
                    half_id.html_path()
                )
                .unwrap();
            }
        }
        writeln!(&mut s, "  }}").unwrap();

        // Graph nodes for Channels.
        {
            writeln!(&mut s, "  {{").unwrap();
            writeln!(
                &mut s,
                "    node [shape=ellipse style=filled fillcolor=green]"
            )
            .unwrap();
            let channels = self.channels.read().unwrap();
            for channel_id in channels.keys().sorted() {
                let channel = channels.get(&channel_id).unwrap();
                writeln!(
                    &mut s,
                    r###"    {} [label="channel-{}\\nm={}, w={}, r={}" URL="{}"]"###,
                    channel_id.dot_id(),
                    channel_id,
                    channel.messages.read().unwrap().len(),
                    channel.reader_count.load(SeqCst),
                    channel.writer_count.load(SeqCst),
                    channel_id.html_path(),
                )
                .unwrap();
            }
            writeln!(&mut s, "  }}").unwrap();
        }
        s
    }

    /// Build a Dot edges stanza for the `ChannelMapping`, allowing for the set
    /// of nodes that have been already observed in the graph.
    #[cfg(feature = "oak_debug")]
    pub fn graph_edges(&self, seen: HashSet<ChannelHalfId>) -> String {
        let mut s = String::new();
        {
            for (half_id, channel_id) in self.writers.read().unwrap().iter() {
                write!(&mut s, "  {} -> {}", half_id.dot_id(), channel_id.dot_id()).unwrap();
                if !seen.contains(half_id) {
                    warn!("reader {:?} is not referenced by any node!", half_id);
                    write!(&mut s, "  [color=red style=bold]").unwrap();
                }
                writeln!(&mut s).unwrap();
            }
        }
        {
            for (half_id, channel_id) in self.readers.read().unwrap().iter() {
                write!(&mut s, "  {} -> {}", channel_id.dot_id(), half_id.dot_id()).unwrap();
                if !seen.contains(half_id) {
                    warn!("writer {:?} is not referenced by any node!", half_id);
                    write!(&mut s, "  [color=red style=bold]").unwrap();
                }
                writeln!(&mut s).unwrap();
            }
        }
        {
            writeln!(&mut s, "  {{").unwrap();
            writeln!(
                &mut s,
                r###"    node [shape=rect fontsize=10 label="msg"]"###
            )
            .unwrap();
            // Messages have no identifier, so just use a count (and don't make it visible to the
            // user).
            let mut msg_counter = 0;
            let channels = self.channels.read().unwrap();
            for channel_id in channels.keys().sorted() {
                let channel = channels.get(&channel_id).unwrap();
                let mut prev_graph_node = channel_id.dot_id();
                for msg in channel.messages.read().unwrap().iter() {
                    msg_counter += 1;
                    let graph_node = format!("msg{}", msg_counter);
                    writeln!(
                        &mut s,
                        "    {} -> {} [style=dashed arrowhead=none]",
                        graph_node, prev_graph_node
                    )
                    .unwrap();
                    for half in &msg.channels {
                        match self.get_direction(*half) {
                            Ok(ChannelHalfDirection::Write) => {
                                writeln!(&mut s, "    {} -> {}", graph_node, half.dot_id())
                                    .unwrap();
                            }
                            Ok(ChannelHalfDirection::Read) => {
                                writeln!(&mut s, "    {} -> {}", half.dot_id(), graph_node)
                                    .unwrap();
                            }
                            Err(_) => {
                                error!(
                                    "message in channel {} has message referencing unknown {:?}",
                                    channel_id, half
                                );
                            }
                        }
                    }
                    prev_graph_node = graph_node;
                }
            }
            writeln!(&mut s, "  }}").unwrap();
        }
        s
    }

    // Build an HTML page describing the `ChannelMapping` structure.
    #[cfg(feature = "oak_debug")]
    pub fn html(&self) -> String {
        let mut s = String::new();
        writeln!(&mut s, "<h2>Channels</h2>").unwrap();
        writeln!(&mut s, "<ul>").unwrap();
        {
            let channels = self.channels.read().unwrap();
            for channel_id in channels.keys().sorted() {
                let channel = channels.get(channel_id).unwrap();
                writeln!(
                    &mut s,
                    r###"<li><a href="{}">channel-{}</a> => {:?}"###,
                    channel_id.html_path(),
                    channel_id,
                    channel
                )
                .unwrap();
            }
        }
        writeln!(&mut s, "</ul>").unwrap();
        writeln!(&mut s, "<h2>Channel Halves</h2>").unwrap();
        writeln!(&mut s, "<ul>").unwrap();
        {
            for (h, channel_id) in self.readers.read().unwrap().iter() {
                writeln!(
                    &mut s,
                    r###"<li><a href="{}">{:?}</a> => READ <a href="{}">channel-{}</a>"###,
                    h.html_path(),
                    h,
                    channel_id.html_path(),
                    channel_id
                )
                .unwrap();
            }
        }
        {
            for (h, channel_id) in self.writers.read().unwrap().iter() {
                writeln!(
                    &mut s,
                    r###"<li><a href="{}">{:?}</a> => WRITE <a href="{}">channel-{}</a>"###,
                    h.html_path(),
                    h,
                    channel_id.html_path(),
                    channel_id
                )
                .unwrap();
            }
        }
        writeln!(&mut s, "</ul>").unwrap();
        s
    }

    // Build an HTML page describing a specific channel identified by `ChannelId`.
    #[cfg(feature = "oak_debug")]
    pub fn html_for_channel(&self, id: u64) -> Option<String> {
        let channel_id: ChannelId = id;
        self.with_channel(channel_id, |channel| Ok(channel.html()))
            .ok()
    }

    // Build an HTML page describing a specific runtime::Handle.
    #[cfg(feature = "oak_debug")]
    pub fn html_for_half(&self, id: u64) -> Option<String> {
        let half_id = ChannelHalfId(id);
        if let Some(channel_id) = self.writers.read().unwrap().get(&half_id) {
            Some(format!(
                r###"WRITE half for <a href="{}">channel {}</a>"###,
                channel_id.html_path(),
                channel_id
            ))
        } else if let Some(channel_id) = self.readers.read().unwrap().get(&half_id) {
            Some(format!(
                r###"READ half for <a href="{}">channel {}</a>"###,
                channel_id.html_path(),
                channel_id
            ))
        } else {
            None
        }
    }
}
