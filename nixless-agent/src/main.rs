use std::path::PathBuf;

use actors::{Downloader, Server, StateKeeper, Unpacker};
use anyhow::anyhow;
use clap::Parser;

mod actors;
mod dbus_connection;
mod fingerprint;
mod path_utils;
mod process_init;
mod signing;
mod state;
mod system_configuration;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Port to listen on.
    #[arg(short, long)]
    port: u16,

    /// Path to the Nix store.
    #[arg(short, long, default_value = "/nix/store")]
    nix_store_path: PathBuf,

    /// Place to temporarily download the files before moving them to the Nix store.
    #[arg(short, long)]
    temp_download_location: PathBuf,

    /// Cache URL.
    #[arg(short, long)]
    cache_url: String,

    /// Cache authorization token. Will be sent in an "Authorization" header on every request.
    #[arg(long)]
    cache_auth_token: Option<String>,

    /// Path where we keep some state about the store and the system.
    #[arg(short, long, default_value = "/nix/var")]
    store_state_directory: PathBuf,
}

#[tokio::main]
async fn async_main(args: Args) -> anyhow::Result<()> {
    let store_path_string = args.nix_store_path.canonicalize()?.to_str().ok_or_else(|| anyhow!("The nix store path given to us can't be represented as an UTF-8 string, but this is required!"))?.to_string();

    let downloader = Downloader::builder()
        .store_path(store_path_string)
        .temp_download_location(args.temp_download_location)
        .cache_url(args.cache_url)
        .cache_auth_token(args.cache_auth_token)
        .build()?;
    let downloader = downloader.start();

    let unpacker = Unpacker::builder()
        .store_path(args.nix_store_path.clone())
        .build()?;
    let unpacker = unpacker.start();

    let store_state = StateKeeper::builder()
        .directory(args.store_state_directory)
        .downloader(downloader)
        .build()?
        .start();

    let server = Server::new(args.port, store_state.child()).start();

    Ok(())
}

// Main is not async because we need to make sure we deal with all the capabilities on the initial thread before we spawn any others.
fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    process_init::ensure_caps()?;

    // TODO: if we detect the nix daemon running, bail here.

    process_init::prepare_nix_store(&args.nix_store_path)?;
    process_init::drop_caps()?;

    async_main(args)
}
