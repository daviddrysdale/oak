/*
 * Copyright 2020 The Project Oak Authors
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

// This module holds C++ functions that are directly linked to and called
// from Rust code (i.e. not via a function pointer).  They are only needed
// for the `rust_main` feature of the `oak_glue` crate; their corresponding
// function prototypes are given (in Rust) in the oak_glue::rust_main module.

#include <cstdint>
#include <memory>

#include "oak/common/logging.h"
#include "oak/proto/application.pb.h"
#include "oak/server/oak_runtime.h"

using ::oak::application::ApplicationConfiguration;

extern "C" {

uintptr_t ffi_init(uint32_t oak_debug, const uint8_t* app_config_data, uintptr_t app_config_len,
                   const uint8_t* pem_root_certs_data, uintptr_t pem_root_certs_len,
                   const uint8_t* private_key_data, uintptr_t private_key_len,
                   const uint8_t* cert_chain_data, uintptr_t cert_chain_len) {
  OAK_LOGGING_INIT("oak_glue", oak_debug);
  OAK_LOG(INFO) << "Start of day initialization";

  if (oak_debug) {
    google::InstallFailureSignalHandler();
  }

  // Reassemble raw memory into C++ types.
  const std::string app_config_raw(reinterpret_cast<const char*>(app_config_data), app_config_len);
  const std::string pem_root_certs(reinterpret_cast<const char*>(pem_root_certs_data),
                                   pem_root_certs_len);
  const std::string private_key(reinterpret_cast<const char*>(private_key_data), private_key_len);
  const std::string cert_chain(reinterpret_cast<const char*>(cert_chain_data), cert_chain_len);
  ApplicationConfiguration app_config;
  app_config.ParseFromString(app_config_raw);

  // Create an OakRuntime object, as an unowned pointer (which can then be passed around over FFI).
  oak::OakRuntime* cpp_runtime = oak::OakRuntime::Create(app_config, pem_root_certs, private_key,
                                                         cert_chain, /* rust_main=*/true)
                                     .release();
  if (cpp_runtime == nullptr) {
    OAK_LOG(FATAL) << "Failed to create C++ OakRuntime object";
  }
  return reinterpret_cast<uintptr_t>(cpp_runtime);
}

void ffi_start_initial_node(uintptr_t cpp_runtime_raw, uint64_t node_id, uint64_t handle) {
  oak::OakRuntime* cpp_runtime = reinterpret_cast<oak::OakRuntime*>(cpp_runtime_raw);
  OAK_LOG(INFO) << "Initial Node startup: node_id=" << node_id << ", handle=" << handle;
  cpp_runtime->RunGrpcNode(node_id, handle);
  OAK_LOG(INFO) << "Initial Node completed";
}

void ffi_stop_initial_node(uintptr_t cpp_runtime_raw) {
  oak::OakRuntime* cpp_runtime = reinterpret_cast<oak::OakRuntime*>(cpp_runtime_raw);
  OAK_LOG(INFO) << "Stop initial Node";
  cpp_runtime->Stop();
}

void ffi_terminate(uintptr_t cpp_runtime_raw) {
  oak::OakRuntime* cpp_runtime = reinterpret_cast<oak::OakRuntime*>(cpp_runtime_raw);
  delete cpp_runtime;
}

}
