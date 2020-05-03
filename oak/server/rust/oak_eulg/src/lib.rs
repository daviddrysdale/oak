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
    #[no_mangle]
    pub fn ffi_init(
        oak_debug: u32,
        app_config_data: *const u8,
        app_config_len: usize,
        pem_root_certs_data: *const u8,
        pem_root_certs_len: usize,
        private_key_data: *const u8,
        private_key_len: usize,
        cert_chain_data: *const u8,
        cert_chain_len: usize,
    ) -> usize;
    #[no_mangle]
    pub fn ffi_start_initial_node(cpp_runtime: usize, node_id: u64, handle: u64);
    #[no_mangle]
    pub fn ffi_stop_initial_node(cpp_runtime: usize);
    #[no_mangle]
    pub fn ffi_terminate(cpp_runtime: usize);
}

/// Initialize the C++ Runtime instance, returning an opaque pointer to the
/// C++ OakRuntime object.
pub fn init(
    debug: bool,
    app_config_data: Vec<u8>,
    pem_root_certs: Vec<u8>,
    private_key: Vec<u8>,
    cert_chain: Vec<u8>,
) -> usize {
    info!("Performing start of day FFI initialization");
    unsafe {
        ffi_init(
            debug as u32,
            app_config_data.as_ptr(),
            app_config_data.len(),
            pem_root_certs.as_ptr(),
            pem_root_certs.len(),
            private_key.as_ptr(),
            private_key.len(),
            cert_chain.as_ptr(),
            cert_chain.len(),
        )
    }
}

/// Start execution of the implicit initial pseudo-Node with the provided
/// initial write handle.  Execution is started in a new thread, whose
/// join handle is returned.
pub fn start_initial_node(
    cpp_runtime: usize,
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
            .spawn(move || ffi_start_initial_node(cpp_runtime, node_id, initial_handle))
            .expect("failed to spawn initial FFI node thread")
    }
}

/// Request that execution stop for the implicit initial pseudo-Node.
pub fn stop_initial_node(cpp_runtime: usize) {
    unsafe {
        ffi_stop_initial_node(cpp_runtime);
    }
}

/// Terminate the C++ runtime.
pub fn terminate(cpp_runtime: usize) {
    unsafe {
        ffi_terminate(cpp_runtime);
    }
    oak_proxy::clear();
}
