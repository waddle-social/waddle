use clap::Parser;
use waddle_xmpp_client::capability_evidence::{run_capability_collector, CapabilityCollectorArgs};

#[tokio::main]
async fn main() {
    if let Err(error) = run_capability_collector(CapabilityCollectorArgs::parse()).await {
        eprintln!("capability collection failed: {error}");
        std::process::exit(1);
    }
}
