mod commands;
mod deck;
mod deeplink;
mod forge;
mod gamelog;
mod registration;
mod settings;
mod ui;

use anyhow::Result;
use deeplink::parse_deeplink;
use log::{error, info};
use registration::RegistrationOutcome;

const SCHEME: &str = "mamoConnector";
const SCHEME_PREFIX: &str = "mamoConnector://";

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        error!("Application error: {err:?}");
    }
}

async fn run() -> Result<()> {
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

    // Handle command if deeplink is present
    let command_result = if let Some(ref dl) = deeplink {
        Some(commands::handle_command(dl).await)
    } else {
        None
    };

    info!("Launching UI with {} arguments", args.len());

    ui::launch(registration, args, deeplink, command_result)?;

    Ok(())
}

fn init_logging() {
    let _ = env_logger::builder().format_timestamp_secs().try_init();
}
