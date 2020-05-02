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

//! Small self-contained pure Rust crate to maintain a global reference
//! to the Rust runtime, for use on the C++/Rust FFI boundary.

use lazy_static::lazy_static;
use oak_runtime::runtime::{NodeId, RuntimeProxy};
use std::sync::RwLock;

lazy_static! {
    static ref PROXY: RwLock<Option<RuntimeProxy>> = RwLock::new(None);
}

const R1: &str = "global proxy lock poisoned";
const R2: &str = "global proxy object missing";

pub fn set_runtime(proxy: RuntimeProxy) {
    *PROXY.write().expect(R1) = Some(proxy);
}

pub fn clear() {
    *PROXY.write().expect(R1) = None;
}

/// Recreate a RuntimeProxy instance that corresponds to the given node ID
/// value.
pub fn for_node(node_id: u64) -> RuntimeProxy {
    PROXY
        .read()
        .expect(R1)
        .as_ref()
        .expect(R2)
        .new_for_node(NodeId(node_id))
}
