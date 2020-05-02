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

#include "oak/common/logging.h"

extern "C" {

void ffi_init() {
  OAK_LOGGING_INIT("oak_glue", true);
  OAK_LOG(INFO) << "Start of day initialization";
}

void ffi_start_initial_node(uint64_t node_id, uint64_t handle) {
  OAK_LOG(INFO) << "Runtime startup: node_id=" << node_id << ", handle=" << handle;
  // Placeholder: should loop here.
}

}
