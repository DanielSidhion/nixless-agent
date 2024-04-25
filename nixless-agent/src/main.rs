use std::path::PathBuf;

use actors::{Downloader, Server, StateKeeper, Unpacker};
use anyhow::anyhow;
use clap::Parser;
use dbus_connection::DBusConnection;
use futures::StreamExt;
use signal_hook::consts::signal;
use signal_hook_tokio::Signals;
use state::AgentState;
use tracing::info;

use crate::process_init::ensure_nix_daemon_not_present;

mod actors;
mod dbus_connection;
mod fingerprint;
mod owned_nar_info;
mod path_utils;
mod process_init;
mod signing;
mod state;
mod system_configuration;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Port to listen on.
    #[arg(long, env = "NIXLESS_AGENT_LISTEN_PORT")]
    port: u16,

    /// Path to the Nix store.
    #[arg(
        long,
        default_value = "/nix/store",
        env = "NIXLESS_AGENT_NIX_STORE_DIR"
    )]
    nix_store_dir: PathBuf,

    /// Path where Nix keeps some state about the store and the system.
    #[arg(long, default_value = "/nix/var", env = "NIXLESS_AGENT_NIX_STATE_DIR")]
    nix_state_dir: PathBuf,

    /// Path where we keep our own state.
    #[arg(
        long,
        default_value = "/var/lib/nixless-agent",
        // This is usually provided by systemd, which is why it doesn't follow the pattern of the other env vars.
        env = "STATE_DIRECTORY"
    )]
    nixless_state_dir: PathBuf,

    /// Place to temporarily download the files before moving them to the Nix store.
    #[arg(long, env = "NIXLESS_AGENT_TEMP_DOWNLOAD_PATH")]
    temp_download_path: PathBuf,

    /// Cache URL.
    #[arg(long, env = "NIXLESS_AGENT_CACHE_URL")]
    cache_url: String,

    /// Cache authorization token. Will be sent in an "Authorization" header on every request.
    #[arg(long, env = "NIXLESS_AGENT_CACHE_AUTH_TOKEN")]
    cache_auth_token: Option<String>,

    /// Public key used by the cache in the format "<key_name>:<encoded_key>".
    #[arg(long, env = "NIXLESS_AGENT_CACHE_PUBLIC_KEY")]
    cache_public_key: Option<String>,

    /// Path to the command used to activate a new system configuration, relative to the configuration top-level package root.
    #[arg(
        long,
        default_value = "bin/switch-to-configuration",
        env = "NIXLESS_AGENT_RELATIVE_CONFIG_ACTIVATION_COMMAND"
    )]
    relative_configuration_activation_command: PathBuf,

    /// Full path to the command used to track configuration activation. This command will be called in the following ways:
    /// - <command> pre-switch <track_directory> <user>
    /// - <command> switch-success <track_directory> <user>
    /// - <command> post-switch <track_directory> <user> <result_code> <exit_code> <exit_status>
    /// Where:
    /// - <track_directory> is the path to the directory where the command should create the tracker files.
    /// - <user> is the username that should be able to read the tracker files.
    /// - <result_code>, <exit_code>, and <exit_status> are passed through from systemd.
    #[arg(long, env = "NIXLESS_AGENT_ABSOLUTE_ACTIVATION_TRACKER_COMMAND")]
    absolute_activation_tracker_command: PathBuf, // TODO: figure out a better way to handle this.
}

async fn handle_signals(mut signals: Signals) {
    while let Some(signal) = signals.next().await {
        match signal {
            signal::SIGHUP => {
                // Reload configuration
                // Reopen the log file
            }
            signal::SIGTERM | signal::SIGINT | signal::SIGQUIT => {
                // Shutdown the system;
                break;
            }
            _ => unreachable!(),
        }
    }
}

#[tokio::main]
async fn async_main(args: Args) -> anyhow::Result<()> {
    let store_path_string = args.nix_store_dir.canonicalize()?.to_str().ok_or_else(|| anyhow!("The nix store path given to us can't be represented as an UTF-8 string, but this is required!"))?.to_string();

    let signals = Signals::new(&[
        signal::SIGHUP,
        signal::SIGTERM,
        signal::SIGINT,
        signal::SIGQUIT,
    ])?;
    let signals_task = tokio::spawn(handle_signals(signals));

    let state = AgentState::from_saved_state_or_new(
        store_path_string.clone(),
        args.nix_state_dir,
        args.nixless_state_dir,
    )
    .await?;

    let dbus_connection = DBusConnection::builder()
        .relative_configuration_activation_command(args.relative_configuration_activation_command)
        .absolute_activation_tracker_command(args.absolute_activation_tracker_command)
        .activation_track_dir(state.absolute_state_path().parent().unwrap().to_path_buf())
        .build()?
        .start();

    let downloader = Downloader::builder()
        .nix_store_dir(store_path_string)
        .temp_download_path(args.temp_download_path)
        .cache_url(args.cache_url)
        .cache_auth_token(args.cache_auth_token)
        .cache_public_key(args.cache_public_key)
        .build()?;
    let downloader = downloader.start();

    let unpacker = Unpacker::builder()
        .nix_store_dir(args.nix_store_dir.clone())
        .build()?;
    let unpacker = unpacker.start();

    let state_keeper = StateKeeper::builder()
        .state(state)
        .dbus_connection(dbus_connection)
        .downloader(downloader)
        .unpacker(unpacker)
        .build()?
        .start();

    let server = Server::new(args.port, state_keeper.child()).start()?;

    signals_task.await?;

    Ok(())
}

// Main is not async because we need to make sure we deal with all the capabilities on the initial thread before we spawn any others.
fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    info!("nixless-agent finished initialising logging, will now proceed with the rest of initialisation.");
    process_init::load_extra_env_file()?;
    let args = Args::parse();

    process_init::ensure_caps()?;
    ensure_nix_daemon_not_present()?;
    process_init::prepare_nix_store(&args.nix_store_dir)?;
    process_init::prepare_nix_state(&args.nix_state_dir)?;
    process_init::drop_caps()?;

    async_main(args)
}
