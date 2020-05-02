/*
 * Copyright 2018 The Project Oak Authors
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

#include <csignal>
#include <memory>
#include <sstream>
#include <string>

#include "absl/flags/flag.h"
#include "absl/flags/parse.h"
#include "absl/synchronization/notification.h"
#include "absl/time/time.h"
#include "oak/common/app_config.h"
#include "oak/common/logging.h"
#include "oak/common/utils.h"
#include "oak/server/oak_runtime.h"

ABSL_FLAG(std::string, application, "", "Application configuration file");
ABSL_FLAG(std::string, ca_cert, "", "Path to the PEM-encoded CA root certificate");
ABSL_FLAG(std::string, private_key, "", "Path to the private key");
ABSL_FLAG(std::string, cert_chain, "", "Path to the PEM-encoded certificate chain");

namespace {

absl::Notification server_done;

void sigint_handler(int) { server_done.Notify(); }

}  // namespace

int main(int argc, char* argv[]) {
  absl::ParseCommandLine(argc, argv);

#ifdef OAK_DEBUG
  // Use the failure signal handler from glog, which prints a stack trace on
  // various crash signals.
  google::InstallFailureSignalHandler();
#endif

  // Load application configuration.
  std::unique_ptr<oak::application::ApplicationConfiguration> application_config =
      oak::ReadConfigFromFile(absl::GetFlag(FLAGS_application));

  // Collect the components needed for server credentials.
  std::string private_key_path = absl::GetFlag(FLAGS_private_key);
  std::string cert_chain_path = absl::GetFlag(FLAGS_cert_chain);
  if (private_key_path.empty()) {
    OAK_LOG(FATAL) << "No private key file specified.";
  }
  if (cert_chain_path.empty()) {
    OAK_LOG(FATAL) << "No certificate chain file specified.";
  }
  std::string private_key = oak::utils::read_file(private_key_path);
  std::string cert_chain = oak::utils::read_file(cert_chain_path);
  std::string ca_cert_path = absl::GetFlag(FLAGS_ca_cert);
  std::string ca_cert = ca_cert_path == "" ? "" : oak::utils::read_file(ca_cert_path);

  // Create the Runtime with the Application.
  OAK_LOG(INFO) << "Creating Oak runtime and application";

  std::unique_ptr<oak::OakRuntime> runtime =
      oak::OakRuntime::Create(*application_config, ca_cert, private_key, cert_chain);
  if (runtime == nullptr) {
    OAK_LOG(ERROR) << "Invalid configuration";
    return 1;
  }

  // Start the runtime.
  OAK_LOG(INFO) << "Starting Oak runtime on port :" << application_config->grpc_port();
  runtime->Start();

  // Wait until notification of signal termination.
  std::signal(SIGINT, sigint_handler);
  server_done.WaitForNotification();

  OAK_LOG(ERROR) << "Terminate Oak Application";
  runtime->Stop();

  return 0;
}
