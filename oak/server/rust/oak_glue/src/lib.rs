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

use byteorder::{ReadBytesExt, WriteBytesExt};
use lazy_static::lazy_static;
use log::{debug, error, info};
use oak_abi::OakStatus;
use oak_runtime::NodeId;
use std::{convert::TryInto, io::Cursor, sync::RwLock};

// Functions that are specific to C++ `main` invoking Rust Runtime.
pub mod cpp_main;

/// `NodeFactory` is the type of a function pointer that the Rust runtime can use
/// to create a pseudo-Node that is implemented in C++.  The thread that calls
/// the function pointer is donated to the pseudo-Node.
type NodeFactory =
    extern "C" fn(data: usize, name: *const u8, name_len: u32, node_id: u64, handle: u64) -> ();

lazy_static! {
    static ref FACTORY: RwLock<Option<(NodeFactory, usize)>> = RwLock::new(None);
}

const F1: &str = "global pseudo-Node factory lock poisoned";
const F2: &str = "global pseudo-Node factory object missing";

/// Register a callback for creating C++ pseudo-Nodes.
///
/// # Safety
///
/// The provided function pointer must be non-NULL and must conform to the
/// `NodeFactory` function signature.
#[no_mangle]
pub unsafe extern "C" fn glue_register_factory(
    factory: Option<crate::NodeFactory>,
    factory_data: usize,
) {
    *FACTORY.write().expect(F1) = Some((factory.expect(F2), factory_data));
    oak_runtime::node::external::register_factory(create_and_run_node);
    info!("register oak_glue::cpp_main::create_and_run_node() as node factory");
}

/// Clear callback used for creating C++ pseudo-Nodes.
#[no_mangle]
pub extern "C" fn glue_unregister_factory() {
    *FACTORY.write().expect(F1) = None;
    info!("C++ factory callback unregistered");
}

/// Method to provide a Rust-compatible wrapper around the registered C++ pseudo-Node
/// factory callback function pointer.
fn create_and_run_node(config_name: &str, node_id: NodeId, handle: oak_abi::Handle) {
    info!(
        "invoke registered factory with '{}', node_id={:?}, handle={}",
        config_name, node_id, handle
    );
    let factory_pair = FACTORY.read().expect(F1);
    let (factory, factory_data) = factory_pair.expect(F2);
    factory(
        factory_data,
        config_name.as_ptr(),
        config_name.len() as u32,
        node_id.0,
        handle,
    );
}

// Methods beyond this point allow C++ pseudo-Nodes to invoke Oak messaging features.
// They are needed regardless of whether `main` is in C++ or Rust.

/// See [`oak_abi::wait_on_channels`].
///
/// # Safety
///
/// The memory range [buf, buf+count*SPACE_BYTES_PER_HANDLE) must be
/// valid.
#[no_mangle]
pub unsafe extern "C" fn glue_wait_on_channels(node_id: u64, buf: *mut u8, count: u32) -> u32 {
    std::panic::catch_unwind(|| {
        debug!(
            "{{{}}}: wait_on_channels(buf={:?}, count={})",
            node_id, buf, count
        );

        // Accumulate the handles we're interested in.
        let size = oak_abi::SPACE_BYTES_PER_HANDLE * count as usize;
        let mut handle_data = Vec::<u8>::with_capacity(size);
        std::ptr::copy_nonoverlapping(buf, handle_data.as_mut_ptr(), size);
        handle_data.set_len(size);
        let mut handles = Vec::with_capacity(count as usize);
        let mut mem_reader = Cursor::new(handle_data);
        for _ in 0..count {
            // Retrieve the 8-byte handle value and skip over the 1 byte status
            // value.
            let handle = mem_reader.read_u64::<byteorder::LittleEndian>().unwrap();
            let _b = mem_reader.read_u8().unwrap();
            debug!("{{{}}}: wait_on_channels   handle {:?}", node_id, handle);
            handles.push(handle);
        }

        let proxy = oak_proxy::for_node(node_id);
        let channel_statuses = match proxy.wait_on_channels(&handles) {
            Ok(r) => r,
            Err(s) => return s as u32,
        };

        if channel_statuses.len() != handles.len() {
            error!(
                "length mismatch: submitted {} handles, got {} results",
                handles.len(),
                channel_statuses.len()
            );
            return OakStatus::ErrInternal as u32;
        }
        for (i, channel_status) in channel_statuses.iter().enumerate() {
            // Write channel status back to the raw pointer.
            let p = buf
                .add(i * oak_abi::SPACE_BYTES_PER_HANDLE + (oak_abi::SPACE_BYTES_PER_HANDLE - 1));

            *p = *channel_status as u8;
        }
        OakStatus::Ok as u32
    })
    .unwrap_or(OakStatus::ErrInternal as u32)
}

/// See [`oak_abi::channel_read`].
///
/// # Safety
///
/// The memory ranges [buf, buf+size) and [handle_buf, handle_buf+handle_count*8) must be
/// valid, as must the raw pointers actual_size and actual_handle_count.
#[no_mangle]
pub unsafe extern "C" fn glue_channel_read(
    node_id: u64,
    handle: u64,
    buf: *mut u8,
    size: usize,
    actual_size: *mut u32,
    handle_buf: *mut u8,
    handle_count: u32,
    actual_handle_count: *mut u32,
) -> u32 {
    debug!(
        "{{{}}}: channel_read(h={}, buf={:?}, size={}, &actual_size={:?}, handle_buf={:?}, count={}, &actual_count={:?})",
        node_id, handle, buf, size, actual_size, handle_buf, handle_count, actual_handle_count
    );

    let proxy = oak_proxy::for_node(node_id);
    let msg = match proxy.channel_try_read_message(handle, size, handle_count as usize) {
        Ok(msg) => msg,
        Err(status) => return status as u32,
    };

    let result = match msg {
        None => {
            *actual_size = 0;
            *actual_handle_count = 0;
            OakStatus::ErrChannelEmpty
        }
        Some(oak_runtime::runtime::NodeReadStatus::Success(msg)) => {
            *actual_size = msg.data.len().try_into().unwrap();
            *actual_handle_count = msg.handles.len().try_into().unwrap();
            std::ptr::copy_nonoverlapping(msg.data.as_ptr(), buf, msg.data.len());
            let mut handle_data = Vec::with_capacity(8 * msg.handles.len());
            for handle in msg.handles {
                debug!("{{{}}}: channel_read() added handle {}", node_id, handle);
                handle_data
                    .write_u64::<byteorder::LittleEndian>(handle)
                    .unwrap();
            }
            std::ptr::copy_nonoverlapping(handle_data.as_ptr(), handle_buf, handle_data.len());
            OakStatus::Ok
        }
        Some(oak_runtime::runtime::NodeReadStatus::NeedsCapacity(a, b)) => {
            *actual_size = a.try_into().unwrap();
            *actual_handle_count = b.try_into().unwrap();
            if a > size as usize {
                OakStatus::ErrBufferTooSmall
            } else {
                OakStatus::ErrHandleSpaceTooSmall
            }
        }
    };

    debug!("{{{}}}: channel_read() -> {:?}", node_id, result);
    result as u32
}

/// See [`oak_abi::channel_write`].
///
/// # Safety
///
/// The memory ranges [buf, buf+size) and [handle_buf, handle_buf+handle_count*8) must be
/// valid.
#[no_mangle]
pub unsafe extern "C" fn glue_channel_write(
    node_id: u64,
    handle: u64,
    buf: *const u8,
    size: usize,
    handle_buf: *const u8,
    handle_count: u32,
) -> u32 {
    debug!(
        "{{{}}}: channel_write(h={}, buf={:?}, size={}, handle_buf={:?}, count={})",
        node_id, handle, buf, size, handle_buf, handle_count
    );
    let mut msg = oak_runtime::NodeMessage {
        data: Vec::with_capacity(size),
        handles: Vec::with_capacity(handle_count as usize),
    };
    std::ptr::copy_nonoverlapping(buf, msg.data.as_mut_ptr(), size);
    msg.data.set_len(size);

    let handle_size = 8 * handle_count as usize;
    let mut handle_data = Vec::<u8>::with_capacity(handle_size);
    std::ptr::copy_nonoverlapping(handle_buf, handle_data.as_mut_ptr(), handle_size);
    handle_data.set_len(handle_size);

    let mut mem_reader = Cursor::new(handle_data);
    for _ in 0..handle_count as isize {
        let handle = mem_reader.read_u64::<byteorder::LittleEndian>().unwrap();
        debug!("{{{}}}: channel_write   include handle {}", node_id, handle);
        msg.handles.push(handle);
    }

    let proxy = oak_proxy::for_node(node_id);
    let result = proxy.channel_write(handle, msg);
    debug!("{{{}}}: channel_write() -> {:?}", node_id, result);
    match result {
        Ok(_) => OakStatus::Ok as u32,
        Err(status) => status as u32,
    }
}

/// See [`oak_abi::channel_create`].
///
/// # Safety
///
/// The raw pointers to memory must be valid.
#[no_mangle]
pub unsafe extern "C" fn glue_channel_create(node_id: u64, write: *mut u64, read: *mut u64) -> u32 {
    debug!("{{{}}}: channel_create({:?}, {:?})", node_id, write, read);
    let proxy = oak_proxy::for_node(node_id);
    let (write_handle, read_handle) =
        proxy.channel_create(&oak_abi::label::Label::public_trusted());
    *write = write_handle;
    *read = read_handle;
    debug!(
        "{{{}}}: channel_create(*w={:?}, *r={:?}) -> OK",
        node_id, write_handle, read_handle
    );
    OakStatus::Ok as u32
}

/// See [`oak_abi::channel_close`].
#[no_mangle]
pub extern "C" fn glue_channel_close(node_id: u64, handle: u64) -> u32 {
    debug!("{{{}}}: channel_close({})", node_id, handle);
    let proxy = oak_proxy::for_node(node_id);
    let result = proxy.channel_close(handle);
    debug!("{{{}}}: channel_close({}) -> {:?}", node_id, handle, result);
    match result {
        Ok(_) => OakStatus::Ok as u32,
        Err(status) => status as u32,
    }
}
