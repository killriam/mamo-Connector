mod commands;
mod deck;
mod deeplink;
mod forge;
mod gamelog;
mod local_server;
mod registration;
mod settings;
mod simulation;
mod ui;

use anyhow::Result;
use deeplink::parse_deeplink;
use log::{error, info, warn};
use registration::RegistrationOutcome;
use std::fs;
use std::path::PathBuf;

const SCHEME: &str = "mamoConnector";
const SCHEME_PREFIX: &str = "mamoConnector://";

/// Get the path to the pending command file
pub fn get_pending_command_path() -> PathBuf {
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("MamoConnector");
    
    // Ensure directory exists
    let _ = fs::create_dir_all(&config_dir);
    
    config_dir.join("pending_command.txt")
}

/// Check if another instance is already running using a lock file
fn check_single_instance() -> Option<std::fs::File> {
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("MamoConnector");
    
    let _ = fs::create_dir_all(&config_dir);
    let lock_path = config_dir.join("instance.lock");
    
    // Try to create/open the lock file with exclusive access
    match fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&lock_path)
    {
        Ok(file) => {
            #[cfg(windows)]
            {
                use std::os::windows::io::AsRawHandle;
                use winapi::um::fileapi::LockFile;
                
                let handle = file.as_raw_handle();
                let locked = unsafe { LockFile(handle as *mut _, 0, 0, 1, 0) };
                
                if locked != 0 {
                    // Successfully locked - we are the primary instance
                    Some(file)
                } else {
                    // Another instance has the lock
                    None
                }
            }
            
            #[cfg(not(windows))]
            {
                // On non-Windows, just use file existence (less robust but works)
                Some(file)
            }
        }
        Err(_) => None,
    }
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        error!("Application error: {err:?}");
    }
}

async fn run() -> Result<()> {
    init_logging();

    info!("Starting Mamo Connector launcher");
    
    let args: Vec<String> = std::env::args().skip(1).collect();
    let deeplink = parse_deeplink(&args, SCHEME_PREFIX);
    
    // Check for single instance
    let lock_file = check_single_instance();
    
    if lock_file.is_none() && deeplink.is_some() {
        // Another instance is running, send the command to it via file
        info!("Another instance is running. Sending command via pending file.");
        
        if let Some(ref dl) = deeplink {
            let pending_path = get_pending_command_path();
            if let Err(e) = fs::write(&pending_path, &dl.raw) {
                error!("Failed to write pending command: {}", e);
            } else {
                info!("Wrote pending command to {:?}", pending_path);
            }
        }
        
        // Exit - the other instance will pick up the command
        return Ok(());
    } else if lock_file.is_none() {
        // Another instance is running but no deeplink - just don't open a new window
        warn!("Another instance is already running. Exiting.");
        return Ok(());
    }
    
    // We are the primary instance - keep the lock file open (it will be held until we exit)
    let _lock = lock_file;
    
    let registration = match registration::ensure_registered(SCHEME) {
        Ok(outcome) => outcome,
        Err(err) => {
            error!("Failed to register custom scheme: {err:?}");
            RegistrationOutcome::failed(err.to_string())
        }
    };

    // Don't process the command here - let the UI handle it with progress logging
    // The UI will switch to Activity tab and show real-time progress
    let command_result: Option<commands::CommandResult> = None;

    // Spawn local simulation HTTP server (loopback only, port 52340)
    local_server::spawn(&tokio::runtime::Handle::current());
    info!("local_server: spawned on port {}", local_server::LOCAL_SIM_PORT);

    info!("Launching UI with {} arguments, deeplink: {:?}", args.len(), deeplink.as_ref().map(|d| &d.raw));

    ui::launch(registration, args, deeplink, command_result)?;

    Ok(())
}

fn init_logging() {
    let _ = env_logger::builder().format_timestamp_secs().try_init();
}
