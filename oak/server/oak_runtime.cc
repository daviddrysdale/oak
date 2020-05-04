/*
 * Copyright 2019 The Project Oak Authors
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

#include "oak/server/oak_runtime.h"

#include <functional>
#include <memory>
#include <string>
#include <thread>

#include "absl/base/call_once.h"
#include "absl/memory/memory.h"
#include "absl/strings/str_cat.h"
#include "include/grpcpp/grpcpp.h"
#include "oak/common/app_config.h"
#include "oak/common/logging.h"
#include "oak/server/grpc_client_node.h"
#include "oak/server/roughtime_client_node.h"
#include "oak/server/rust/oak_glue/oak_glue.h"
#include "oak/server/storage/storage_node.h"

namespace oak {

namespace {
// Name to use for the (sole) gRPC server pseudo-Node.  This will not clash with
// any dynamically created Node names because they are all of the form
// "<config>-<number>".
constexpr char kGrpcNodeName[] = "grpc";

absl::once_flag glue_once;

void NodeFactory(uintptr_t data, const char* name, uint32_t name_len, uint64_t node_id,
                 uint64_t handle) {
  OakRuntime* runtime = reinterpret_cast<OakRuntime*>(data);
  std::string config_name(name, name_len);
  runtime->CreateAndRunPseudoNode(config_name, node_id, handle);
}

// Helper to build gRPC server credentials from certificates and keys.
std::shared_ptr<grpc::ServerCredentials> BuildTlsCredentials(std::string pem_root_certs,
                                                             std::string private_key,
                                                             std::string cert_chain) {
  grpc::SslServerCredentialsOptions::PemKeyCertPair key_cert_pair = {private_key, cert_chain};
  grpc::SslServerCredentialsOptions options;
  options.pem_root_certs = pem_root_certs;
  options.pem_key_cert_pairs.push_back(key_cert_pair);
  return grpc::SslServerCredentials(options);
}

}  // namespace

std::unique_ptr<OakRuntime> OakRuntime::Create(const application::ApplicationConfiguration& config,
                                               const std::string& pem_root_certs,
                                               const std::string& private_key,
                                               const std::string& cert_chain, bool rust_main) {
#ifdef OAK_DEBUG
  // If main() is in Rust, don't (re-)initialize debugging.
  bool debug_mode = !rust_main;
#else
  bool debug_mode = false;
#endif
  absl::call_once(glue_once, &glue_init, static_cast<uint32_t>(debug_mode));

  if (!ValidApplicationConfig(config)) {
    OAK_LOG(ERROR) << "Invalid configuration";
    return nullptr;
  }

  std::shared_ptr<grpc::ServerCredentials> grpc_credentials =
      BuildTlsCredentials(pem_root_certs, private_key, cert_chain);
  return std::unique_ptr<OakRuntime>(new OakRuntime(config, grpc_credentials, rust_main));
}

OakRuntime::OakRuntime(const application::ApplicationConfiguration& config,
                       std::shared_ptr<grpc::ServerCredentials> grpc_credentials, bool rust_main)
    : rust_main_(rust_main), grpc_handle_(kInvalidHandle) {
  // Accumulate the various data structures indexed by config name.
  for (const auto& node_config : config.node_configs()) {
    if (node_config.has_storage_config()) {
      const application::StorageProxyConfiguration& storage_config = node_config.storage_config();
      storage_config_[node_config.name()] =
          absl::make_unique<std::string>(storage_config.address());
    } else if (node_config.has_grpc_client_config()) {
      const application::GrpcClientConfiguration& grpc_config = node_config.grpc_client_config();
      grpc_client_config_[node_config.name()] =
          absl::make_unique<std::string>(grpc_config.address());
    } else if (node_config.has_roughtime_client_config()) {
      roughtime_client_config_[node_config.name()] =
          absl::make_unique<application::RoughtimeClientConfiguration>(
              node_config.roughtime_client_config());
    }
  }
  std::string config_data;
  if (!config.SerializeToString(&config_data)) {
    OAK_LOG(FATAL) << "Failed to serialize ApplicationConfiguration";
  }

  // Create the gRPC server pseudo-Node instance.
  grpc_node_ = OakGrpcNode::Create(kGrpcNodeName, grpc_credentials, config.grpc_port());

  OAK_LOG(INFO) << "Registering NodeFactory";
  glue_register_factory(NodeFactory, reinterpret_cast<uintptr_t>(this));

  if (!rust_main) {
    // If main() is in C++, kick off the Rust runtime and get back from it
    // the NodeID and initial handle to use for the gRPC server pseudo-Node.
    OAK_LOG(INFO) << "Starting Rust runtime";
    uint64_t grpc_node_id;
    grpc_handle_ = glue_start(reinterpret_cast<const uint8_t*>(config_data.data()),
                              static_cast<uint32_t>(config_data.size()), &grpc_node_id);
    grpc_node_->SetNodeId(grpc_node_id);
    OAK_LOG(INFO) << "Started Rust runtime";
  }
}

OakRuntime::~OakRuntime() {
  OAK_LOG(INFO) << "Unregistering NodeFactory";
  glue_unregister_factory();
}

void OakRuntime::RunGrpcNode(uint64_t node_id, Handle handle) {
  OAK_LOG(INFO) << "Set gRPC Node info: node_id=" << node_id << ", handle=" << handle;
  grpc_handle_ = handle;
  grpc_node_->SetNodeId(node_id);
  OAK_LOG(INFO) << "Running gRPC Node";
  grpc_node_->Run(grpc_handle_);
}

// Create (but don't start) a new Node instance.  Return a borrowed pointer to
// the new Node (or nullptr on failure).
std::unique_ptr<OakNode> OakRuntime::CreateNode(const std::string& config_name,
                                                NodeId node_id) const {
  std::string name = absl::StrCat(config_name, "-", node_id);

  auto storage_iter = storage_config_.find(config_name);
  if (storage_iter != storage_config_.end()) {
    std::string address = *(storage_iter->second.get());
    OAK_LOG(INFO) << "Create storage proxy node named {" << name << "} connecting to " << address;
    return absl::make_unique<StorageNode>(name, node_id, address);
  }

  auto grpc_client_iter = grpc_client_config_.find(config_name);
  if (grpc_client_iter != grpc_client_config_.end()) {
    std::string address = *(grpc_client_iter->second.get());
    OAK_LOG(INFO) << "Create gRPC client node named {" << name << "} connecting to " << address;
    return absl::make_unique<GrpcClientNode>(name, node_id, address);
  }

  auto roughtime_iter = roughtime_client_config_.find(config_name);
  if (roughtime_iter != roughtime_client_config_.end()) {
    const application::RoughtimeClientConfiguration* roughtime_config =
        roughtime_iter->second.get();
    OAK_LOG(INFO) << "Create Roughtime client node named {" << name << "} with config "
                  << roughtime_config->DebugString();
    return absl::make_unique<RoughtimeClientNode>(name, node_id, *roughtime_config);
  }

  OAK_LOG(ERROR) << "failed to find config with name " << config_name;
  return nullptr;
}

void OakRuntime::CreateAndRunPseudoNode(const std::string& config_name, NodeId node_id,
                                        Handle handle) const {
  std::unique_ptr<OakNode> node = CreateNode(config_name, node_id);
  if (node == nullptr) {
    OAK_LOG(FATAL) << "Failed to create pseudo-Node with config " << config_name;
  }

  OAK_LOG(INFO) << "Start pseudo-node of config '" << config_name << "' with initial handle "
                << handle;
  node->Run(handle);
  OAK_LOG(INFO) << "Finished pseudo-node of config '" << config_name << "' with initial handle "
                << handle;
}

void OakRuntime::Start() const {
  OAK_LOG(INFO) << "Starting runtime";
  // Start the initial gRPC Node running.
  grpc_node_->Start(grpc_handle_);
}

void OakRuntime::Stop() const {
  OAK_LOG(INFO) << "Stopping gRPC server pseudo-Node...";
  grpc_node_->Stop();
  if (!rust_main_) {
    OAK_LOG(INFO) << "Stopping Rust runtime...";
    glue_stop();
  }
}

}  // namespace oak
