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
use log::{debug, error, info};
use oak_abi::OakStatus;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt::Write,
    sync::{
        atomic::{AtomicU64, Ordering::SeqCst},
        Arc, Mutex, RwLock, RwLockWriteGuard, Weak,
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
///
/// Channels are reference counted using `Arc<Channel>`, always in the form of a
/// `ChannelHalf` object that references one end of the `Channel` (read or write) and which
/// is included in the `reader_count` or `writer_count`.  The `ChannelMapping` object also
/// holds a `Weak` reference to the `Channel`, to allow garbage collection and introspection.
pub struct Channel {
    // The owning `ChannelMapping`.
    owner: Weak<ChannelMapping>,

    // The identifier under which this channel is tracked in the main `ChannelMapping`.
    id: ChannelId,

    pub messages: RwLock<Messages>,

    // Counts of the numbers of `ChannelHalf` objects that refer to this channel.
    writer_count: AtomicU64,
    reader_count: AtomicU64,

    /// A HashMap of `ThreadId`s to `Weak<Thread>`s. This allows threads to insert a weak reference
    /// to themselves to be woken when a new message is available. Weak references are used so that
    /// if the thread is woken by a different channel, it can deallocate the underlying `Arc`
    /// instead of removing itself from all the `Channel`s it subscribed to.
    /// Threads can be woken up spuriously without issue.
    waiting_threads: WaitingThreads,

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
            "Channel {{ id={}, #readers={}, #writers={}, label={:?} }}",
            self.id,
            self.reader_count.load(SeqCst),
            self.writer_count.load(SeqCst),
            self.label,
        )
    }
}

/// A reference to one half of a [`Channel`].
pub struct ChannelHalf {
    channel: Arc<Channel>,
    pub direction: ChannelHalfDirection,
}

impl ChannelHalf {
    // Constructor for `ChannelHalf` keeps the underlying channel's reader/writer count
    // up-to-date.
    pub fn new(channel: Arc<Channel>, direction: ChannelHalfDirection) -> Self {
        match direction {
            ChannelHalfDirection::Write => channel.inc_writer_count(),
            ChannelHalfDirection::Read => channel.inc_reader_count(),
        };
        ChannelHalf { channel, direction }
    }
}

// Manual implementation of the `Clone` trait to keep the counts for the underlying channel in sync.
impl Clone for ChannelHalf {
    fn clone(&self) -> Self {
        ChannelHalf::new(self.channel.clone(), self.direction)
    }
}

// Manual implementation of the `Drop` trait to keep the counts for the underlying channel in sync.
// Note that a non-ephemeral `ChannelHalf` (i.e. one that's stored in a data structure, rather than
// born and dying in a transient scope) should be dropped by calling `ChannelMapping::drop_half()`,
// so that if the final `ChannelHalf` that refers to a `Channel` is dropped, then the
// `ChannelMapping` can remove its own raw `Arc<Channel>` and allow the `Channel` to drop.
impl Drop for ChannelHalf {
    fn drop(&mut self) {
        match self.direction {
            ChannelHalfDirection::Write => self.channel.dec_writer_count(),
            ChannelHalfDirection::Read => self.channel.dec_reader_count(),
        };

        if self.direction == ChannelHalfDirection::Write
            && self.channel.has_readers()
            && !self.channel.has_writers()
        {
            // This was the last writer to the channel: wake any waiters so they
            // can be aware that the channel is orphaned.
            self.channel.wake_waiters();
        }
    }
}

impl std::fmt::Debug for ChannelHalf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Channel {} {}",
            self.channel.id,
            match self.direction {
                ChannelHalfDirection::Write => "WRITE",
                ChannelHalfDirection::Read => "READ",
            },
        )
    }
}

impl DotIdentifier for ChannelHalf {
    fn dot_id(&self) -> String {
        self.channel.id.dot_id()
    }
}

impl HtmlPath for ChannelHalf {
    fn html_path(&self) -> String {
        self.channel.id.html_path()
    }
}

/// Perform an operation on a [`Channel`] associated with a [`ChannelId`].
fn with_channel<U, F: FnOnce(Arc<Channel>) -> Result<U, OakStatus>>(
    half: &ChannelHalf,
    f: F,
) -> Result<U, OakStatus> {
    f(half.channel.clone())
}

/// Perform an operation on a [`Channel`] associated with a reader [`ChannelHalf`].
pub fn with_reader_channel<U, F: FnOnce(Arc<Channel>) -> Result<U, OakStatus>>(
    half: &ChannelHalf,
    f: F,
) -> Result<U, OakStatus> {
    if half.direction != ChannelHalfDirection::Read {
        return Err(OakStatus::ErrBadHandle);
    }
    with_channel(half, f)
}

/// Perform an operation on a [`Channel`] associated with a writer [`ChannelHalf`].
pub fn with_writer_channel<U, F: FnOnce(Arc<Channel>) -> Result<U, OakStatus>>(
    half: &ChannelHalf,
    f: F,
) -> Result<U, OakStatus> {
    if half.direction != ChannelHalfDirection::Write {
        return Err(OakStatus::ErrBadHandle);
    }
    with_channel(half, f)
}

/// The direction of a [`ChannelHalf`].
#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug)]
pub enum ChannelHalfDirection {
    Read,
    Write,
}

/// An internal identifier to track a [`Channel`].
pub type ChannelId = u64;

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

/// Mapping of [`ChannelId`]s to [`Channel`]s.
pub struct ChannelMapping {
    // The `channels` field hold a `Weak` reference to a `Channel`, to allow
    // introspection and to allow leaked cycles to be garbage collected.
    channels: RwLock<HashMap<ChannelId, Weak<Channel>>>,
    next_channel_id: AtomicU64,
}

impl Drop for Channel {
    fn drop(&mut self) {
        debug!("dropping Channel object {:?}", self);
        // There should be no waiters for this channel (a waiting Node would have
        // to have a `Handle` to wait on, which would be a reference that pins this
        // channel to existence) so no need to `wake_waiters()`.
        // Deliberately clear the HashMap under the lock to avoid a TSAN report.
        self.waiting_threads.lock().unwrap().clear();

        // Drop any weak reference from the owner's `HashMap` too.
        if let Some(mapping) = self.owner.upgrade() {
            let mut channels = mapping.channels.write().unwrap();
            channels.remove(&self.id);
            debug!("Channel::drop: removed channel {:?} from tracking", self.id);
        }
    }
}

impl Channel {
    fn new(
        owner: Weak<ChannelMapping>,
        id: ChannelId,
        label: &oak_abi::label::Label,
    ) -> Arc<Channel> {
        debug!("create new Channel object with ID {}", id);
        Arc::new(Channel {
            owner,
            id,
            messages: RwLock::new(Messages::new()),
            writer_count: AtomicU64::new(0),
            reader_count: AtomicU64::new(0),
            waiting_threads: Mutex::new(HashMap::new()),
            label: label.clone(),
        })
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

    /// Decrement the [`Channel`] writer counter.
    fn dec_writer_count(&self) {
        if self.writer_count.fetch_sub(1, SeqCst) == 0 {
            panic!("remove_reader: Writer count was already 0, something is very wrong!")
        }
    }

    /// Decrement the [`Channel`] reader counter.
    fn dec_reader_count(&self) {
        if self.reader_count.fetch_sub(1, SeqCst) == 0 {
            panic!("remove_reader: Reader count was already 0, something is very wrong!")
        }
    }

    /// Increment the [`Channel`] writer counter.
    fn inc_writer_count(&self) -> u64 {
        self.writer_count.fetch_add(1, SeqCst)
    }

    /// Increment the [`Channel`] reader counter.
    fn inc_reader_count(&self) -> u64 {
        self.reader_count.fetch_add(1, SeqCst)
    }

    // Build an HTML page describing a specific `Channel`.
    fn html(&self) -> String {
        let mut s = String::new();
        write!(
            &mut s,
            "Channel {{ id={}, reader_count={}, writer_count={}, label={:?} }}<br/>",
            self.id,
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

    /// Insert the given `thread` reference into `thread_id` slot of the HashMap of waiting
    /// channels attached to an underlying channel. This allows the channel to wake up any waiting
    /// channels by calling `thread::unpark` on all the threads it knows about.
    pub fn add_waiter(&self, thread_id: ThreadId, thread: &Arc<Thread>) {
        self.waiting_threads
            .lock()
            .unwrap()
            .insert(thread_id, Arc::downgrade(thread));
    }

    pub fn wake_waiters(&self) {
        // Unpark (wake up) all waiting threads that still have live references. The first thread
        // woken can immediately read the message, and others might find `messages` is empty before
        // they are even woken. This should not be an issue (being woken does not guarantee a
        // message is available), but it could potentially result in some particular thread always
        // getting first chance to read the message.
        //
        // If a thread is woken and finds no message it will take the `waiting_threads` lock and
        // add itself again. Note that since that lock is currently held, the woken thread will add
        // itself to waiting_threads *after* we call clear below as we release the lock implicilty
        // on leaving this function.
        let mut waiting_threads = self.waiting_threads.lock().unwrap();
        for thread in waiting_threads.values() {
            if let Some(thread) = thread.upgrade() {
                thread.unpark();
            }
        }
        waiting_threads.clear();
    }
}

impl ChannelMapping {
    /// Create a new empty [`ChannelMapping`].
    pub fn new() -> ChannelMapping {
        ChannelMapping {
            channels: RwLock::new(HashMap::new()),
            next_channel_id: AtomicU64::new(0),
        }
    }

    /// Give access to the internals of the channel mapping.
    pub fn get_channels<'a>(&'a self) -> RwLockWriteGuard<'a, HashMap<ChannelId, Weak<Channel>>> {
        self.channels.write().unwrap()
    }

    /// Creates a new [`Channel`] and returns a `(writer half, reader half)` pair.
    pub fn new_channel(
        self: &Arc<Self>,
        label: &oak_abi::label::Label,
    ) -> (ChannelHalf, ChannelHalf) {
        let channel_id = self.next_channel_id.fetch_add(1, SeqCst);
        let channel = Channel::new(Arc::downgrade(&self), channel_id, label);
        let result = (
            ChannelHalf::new(channel.clone(), ChannelHalfDirection::Write),
            ChannelHalf::new(channel.clone(), ChannelHalfDirection::Read),
        );
        debug!("tracking new channel ID {}", channel_id);
        self.channels
            .write()
            .unwrap()
            .insert(channel_id, Arc::downgrade(&channel));
        result
    }

    /// Unblock all threads waiting on any channel.
    pub fn notify_all_waiters(&self) {
        for channel in self
            .channels
            .read()
            .expect("could not acquire channel mapping")
            .values()
        {
            if let Some(channel) = channel.upgrade() {
                channel.wake_waiters();
            }
        }
    }

    // Visit the `Channel` referenced by a `ChannelHalf`, updating the given set of
    // as-yet unvisited `ChannelId` values.
    pub fn visit_half(&self, unvisited: &mut HashSet<ChannelId>, half: &ChannelHalf) {
        let channel_id = half.channel.id;
        if !unvisited.contains(&channel_id) {
            debug!("    already visited {}, move on", channel_id);
            return;
        }
        debug!("    visit {}, check its messages", channel_id);
        unvisited.remove(&channel_id);

        // Recurse: visit all `ChannelHalf` objects that are accessible from the messages
        // held in this channel.
        for msg in half.channel.messages.read().unwrap().iter() {
            for half in &msg.channels {
                self.visit_half(unvisited, half);
            }
        }
    }

    pub fn gc(&self, unvisited: HashSet<ChannelId>) -> String {
        let mut s = "<h2>Channel GC Result</h2>".to_string();
        for channel_id in unvisited {
            let result = self.gc_channel(channel_id);
            write!(&mut s, "<p>Reaped Channel {}: {}", channel_id, result).unwrap();
        }
        info!(
            "channel count after GC done: {}",
            self.channels.read().unwrap().len()
        );
        s
    }

    fn gc_channel(&self, channel_id: ChannelId) -> String {
        let channel_opt = { self.channels.write().unwrap().remove(&channel_id) };
        if let Some(channel_weak) = channel_opt {
            if let Some(channel) = channel_weak.upgrade() {
                debug!(
                    "drop unreachable channel {:?} with {} Arcs remaining",
                    channel,
                    Arc::strong_count(&channel)
                );

                // Perform the same actions as in `ChannelHalf::drop()`, but while
                // holding the write lock and without bothering to track.
                // `ChannelHalf` counts
                {
                    channel.messages.write().unwrap().clear();
                }
                drop(channel); // Be explicit: ref dropped here.
                "dropped".to_string()
            } else {
                error!(
                    "Unvisited channel ID {} present in ChannelMapping but weak ref invalid",
                    channel_id
                );
                "found dangling weak ref".to_string()
            }
        } else {
            error!(
                "Unvisited channel ID {} not present in ChannelMapping!",
                channel_id
            );
            "not found".to_string()
        }
    }

    /// Build a Dot nodes stanza for the `ChannelMapping`.
    #[cfg(feature = "oak_debug")]
    pub fn graph_nodes(&self) -> String {
        let mut s = String::new();
        {
            writeln!(&mut s, "  {{").unwrap();
            writeln!(
                &mut s,
                "    node [shape=ellipse style=filled fillcolor=green]"
            )
            .unwrap();
            let channels = self.channels.read().unwrap();
            for channel_id in channels.keys().sorted() {
                let channel_weak = channels.get(&channel_id).unwrap();
                if let Some(channel) = channel_weak.upgrade() {
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
                } else {
                    writeln!(
                        &mut s,
                        r###"    {} [label="channel-{}\\nweak-only" URL="{}"]"###,
                        channel_id.dot_id(),
                        channel_id,
                        channel_id.html_path(),
                    )
                    .unwrap();
                }
            }
            writeln!(&mut s, "  }}").unwrap();
        }
        s
    }

    /// Build a Dot edges stanza for the `ChannelMapping`.
    #[cfg(feature = "oak_debug")]
    pub fn graph_edges(&self) -> String {
        let mut s = String::new();
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
                let channel_weak = channels.get(&channel_id).unwrap();
                if let Some(channel) = channel_weak.upgrade() {
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
                            match half.direction {
                                ChannelHalfDirection::Write => {
                                    writeln!(&mut s, "    {} -> {}", graph_node, half.dot_id())
                                        .unwrap();
                                }
                                ChannelHalfDirection::Read => {
                                    writeln!(&mut s, "    {} -> {}", half.dot_id(), graph_node)
                                        .unwrap();
                                }
                            }
                        }
                        prev_graph_node = graph_node;
                    }
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
        s
    }

    // Build an HTML page describing a specific channel identified by `ChannelId`.
    #[cfg(feature = "oak_debug")]
    pub fn html_for_channel(&self, id: u64) -> Option<String> {
        let channel_id: ChannelId = id;
        {
            let channels = self.channels.read().unwrap();
            let channel_weak = channels.get(&channel_id)?.clone();
            match channel_weak.upgrade() {
                None => Some(format!("Weak reference to Channel {{ id={} }}", channel_id)),
                Some(channel) => Some(channel.html()),
            }
        }
    }
}
