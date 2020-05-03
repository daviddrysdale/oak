#
# Copyright 2019 The Project Oak Authors
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
#

load("@io_bazel_rules_rust//rust:rust.bzl", "rust_library")
load("@rules_cc//cc:defs.bzl", "cc_library")

package(
    default_visibility = ["//oak/server:__subpackages__"],
    licenses = ["notice"],
)

rust_library(
    name = "oak_eulg",
    srcs = glob(["src/**/*.rs"]),
    crate_features = ["rust_main"],
    crate_type = "rlib",
    edition = "2018",
    deps = [
        ":oak_eulg_cpp",
        "//cargo:log",
        "//oak/server/rust/oak_abi",
        "//oak/server/rust/oak_proxy",
        "//oak/server/rust/oak_runtime",
    ],
)

cc_library(
    name = "oak_eulg_cpp",
    srcs = ["oak_eulg.cc"],
    deps = [
        "//oak/common:logging",
        "//oak/proto:application_cc_proto",
        "//oak/server:oak_runtime",
    ],
)
