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

//! Functions that are specific to Rust `main` invoking C++ functionality.

use log::info;
use oak_runtime::runtime::RuntimeProxy;

extern "C" {
    pub fn ffi_init();
    pub fn ffi_start_initial_node(node_id: u64, handle: u64);
}

pub fn init() {
    info!("Performing start of day FFI initialization");
    unsafe {
        ffi_init();
    }
}

pub fn start_initial_node(
    proxy: RuntimeProxy,
    initial_handle: oak_abi::Handle,
) -> std::thread::JoinHandle<()> {
    let node_id = proxy.node_id.0;
    info!(
        "{:?} starting initial implicit Node with initial write handle {}",
        node_id, initial_handle
    );

    // Store the Runtime instance with the C++->Rust glue, so any invocations
    // of messaging functionality by C++ pseudo-Nodes can reach the Runtime.
    oak_proxy::set_runtime(proxy);

    // Donate a thread to the C++ code to run the initial Node on.
    unsafe {
        std::thread::Builder::new()
            .name("initial-ffi-node".to_string())
            .spawn(move || ffi_start_initial_node(node_id, initial_handle))
            .expect("failed to spawn initial FFI node thread")
    }
}

pub fn stop() {
    oak_proxy::clear();
}
