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

//! Functions that are specific to C++ `main` invoking Rust Runtime.

use log::{error, info, warn};
use oak_runtime::proto::oak::application::ApplicationConfiguration;
use prost::Message;

/// Perform once-only start of day initialization.
#[no_mangle]
pub extern "C" fn glue_init(debug: u32) {
    let _ = ::std::panic::catch_unwind(|| {
        if debug != 0 {
            simple_logger::init_with_level(log::Level::Debug).expect("failed to initialize logger");
        }
        info!("Rust FFI glue initialized");
    });
}

/// Start the Rust runtime, with the ApplicationConfiguration provided in
/// serialized form.
///
/// # Safety
///
/// Caller must ensure that the memory range [config_buf, config_buf+config_len) is
/// accessible and holds a protobuf-serialized ApplicationConfiguration message.
/// The location provided to receive the `node_id` must be valid and writable.
#[no_mangle]
pub unsafe extern "C" fn glue_start(
    config_buf: *const u8,
    config_len: u32,
    node_id: *mut u64,
) -> u64 {
    std::panic::catch_unwind(|| {
        let config_data = std::slice::from_raw_parts(config_buf, config_len as usize);

        let app_config = match ApplicationConfiguration::decode(config_data) {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to decode ApplicationConfiguration: {}", e);
                return oak_abi::INVALID_HANDLE;
            }
        };
        let runtime_config = oak_runtime::RuntimeConfiguration {
            metrics_port: Some(3030),
            introspect_port: Some(1909),
        };
        info!(
            "start runtime with initial config {}.{} {:?}",
            app_config.initial_node_config_name, app_config.initial_entrypoint_name, runtime_config
        );

        // Configure the Rust Runtime, and run the gRPC server pseudo-Node as the implicit
        // initial Node.
        let (grpc_proxy, grpc_handle) =
            match oak_runtime::configure_and_run(app_config, runtime_config) {
                Ok(p) => p,
                Err(status) => {
                    error!("Failed to start runtime: {:?}", status);
                    return oak_abi::INVALID_HANDLE;
                }
            };
        *node_id = grpc_proxy.node_id.0;
        info!(
            "runtime started, grpc_node_id={}, grpc_handle={}",
            *node_id, grpc_handle
        );

        oak_proxy::set_runtime(grpc_proxy);
        grpc_handle
    })
    .unwrap_or(oak_abi::INVALID_HANDLE)
}

/// Stop the Rust runtime.
#[no_mangle]
pub extern "C" fn glue_stop() {
    let runtime = oak_proxy::for_node(0);
    info!("runtime graph at exit:\n\n{}", runtime.graph_runtime());
    warn!("stopping Rust runtime");
    runtime.stop_runtime();
    oak_proxy::clear();
}
