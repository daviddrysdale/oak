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

//! An utility binary to run Oak Runtime.
//!
//! To invoke, run the following command from the root of the repository:
//!
//! ```
//! cargo run --package=oak_loader -- --application=<APP_CONFIG_PATH>
//! ```

use log::info;
use oak_runtime::{configure_and_run, proto::oak::application::ApplicationConfiguration};
use prost::Message;
use std::{
    fs::File,
    io::Read,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use structopt::StructOpt;

#[derive(StructOpt, Clone)]
#[structopt(about = "Oak Loader")]
pub struct Opt {
    #[structopt(long, help = "Application configuration file")]
    application: String,
    // TODO(#806): Make arguments non-optional once TLS support is enabled.
    #[structopt(long, help = "Path to the PEM-encoded CA root certificate")]
    ca_cert: Option<String>,
    #[structopt(long, help = "Path to the private key")]
    private_key: Option<String>,
    #[structopt(long, help = "Path to the PEM-encoded certificate chain")]
    cert_chain: Option<String>,
    #[structopt(
        long,
        default_value = "3030",
        help = "Metrics server port number. Defaults to 3030."
    )]
    metrics_port: u16,
    #[structopt(long, help = "Starts the Runtime without a metrics server.")]
    no_metrics: bool,
    #[structopt(
        long,
        default_value = "1909",
        help = "Introspection server port number. Defaults to 1909."
    )]
    introspect_port: u16,
    #[structopt(long, help = "Starts the Runtime without an introspection server.")]
    no_introspect: bool,
}

fn read_file(filename: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut file = File::open(filename)
        .map_err(|error| format!("Failed to open file <{}>: {:?}", filename, error))?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)
        .map_err(|error| format!("Failed to read file <{}>: {:?}", filename, error))?;
    Ok(buffer)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    simple_logger::init().expect("failed to initialize logger");
    let opt = Opt::from_args();

    let app_config_data = read_file(&opt.application)?;
    let app_config = ApplicationConfiguration::decode(&app_config_data[..])?;
    let runtime_config = oak_runtime::RuntimeConfiguration {
        metrics_port: if opt.no_metrics {
            None
        } else {
            Some(opt.metrics_port)
        },
        introspect_port: if opt.no_introspect {
            None
        } else {
            Some(opt.introspect_port)
        },
    };

    // Start the Runtime from the given config.
    info!("starting Runtime");
    let (runtime, initial_handle) = configure_and_run(app_config, runtime_config)
        .map_err(|status| format!("status {:?}", status))?;
    info!(
        "initial node {:?} with write handle {:?}",
        runtime.node_id, initial_handle
    );

    let done = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::SIGINT, Arc::clone(&done))?;

    // The Runtime kicks off its own threads for the initial Node and any subsequently created
    // Nodes, so just block the current thread until a signal arrives.
    while !done.load(Ordering::Relaxed) {
        // There are few synchronization mechanisms that are allowed to be used in a signal
        // handler context, so use a primitive sleep loop to watch for the termination
        // notification (rather than something more accurate like `std::sync::Condvar`).
        // See e.g.: http://man7.org/linux/man-pages/man7/signal-safety.7.html
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    info!("stop Runtime");
    runtime.stop_runtime();
    Ok(())
}
