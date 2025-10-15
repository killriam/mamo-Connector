mod deeplink;
mod registration;
mod ui;

use anyhow::Result;
use deeplink::parse_deeplink;
use log::{error, info};
use registration::RegistrationOutcome;

const SCHEME: &str = "mamoConnector";
const SCHEME_PREFIX: &str = "mamoConnector://";

fn main() {
    if let Err(err) = run() {
        error!("Application error: {err:?}");
    }
}

fn run() -> Result<()> {
    init_logging();

    info!("Starting Mamo Connector launcher");
    let registration = match registration::ensure_registered(SCHEME) {
        Ok(outcome) => outcome,
        Err(err) => {
            error!("Failed to register custom scheme: {err:?}");
            RegistrationOutcome::failed(err.to_string())
        }
    };

    let args: Vec<String> = std::env::args().skip(1).collect();
    let deeplink = parse_deeplink(&args, SCHEME_PREFIX);

    info!("Launching UI with {} arguments", args.len());

    ui::launch(registration, args, deeplink)?;

    Ok(())
}

fn init_logging() {
    let _ = env_logger::builder().format_timestamp_secs().try_init();
}
