use std::{collections::HashSet, path::PathBuf};

use actix_web::{
    error::InternalError, guard::fn_guard, http::StatusCode, web, App, HttpRequest, HttpResponse,
    HttpServer, Responder,
};
use anyhow::anyhow;
use clap::Parser;
use downloader::{Downloader, StartedDownloader};
use signing::CachePublicKeychain;
use unpacker::{StartedUnpacker, Unpacker};

mod downloader;
mod fingerprint;
mod process_init;
mod signing;
mod unpacker;

async fn entry_point(
    req: HttpRequest,
    payload_string: String,
    downloader: web::Data<StartedDownloader>,
    unpacker: web::Data<StartedUnpacker>,
) -> actix_web::Result<impl Responder> {
    let paths: Vec<_> = payload_string.lines().map(String::from).collect();

    let download_results = downloader
        .download_paths(paths)
        .await
        .map_err(|err| InternalError::new(err, StatusCode::INTERNAL_SERVER_ERROR))?;

    unpacker
        .unpack_downloads(download_results)
        .await
        .map_err(|err| InternalError::new(err, StatusCode::INTERNAL_SERVER_ERROR))?;

    Ok(HttpResponse::NoContent())
}

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
}

async fn probe_nix_store(store_path: &PathBuf) -> anyhow::Result<HashSet<String>> {
    let mut entries = tokio::fs::read_dir(store_path).await?;
    let mut path_set = HashSet::new();

    while let Some(entry) = entries.next_entry().await? {
        if entry.file_type().await?.is_dir() {
            if let Some(path_str) = entry.path().to_str() {
                path_set.insert(path_str.to_string());
            } else {
                return Err(anyhow!("found a path in the store containing non-UTF-8 characters, which is unexpected: {}", entry.path().to_string_lossy()));
            }
        } else {
            return Err(anyhow!(
                "found a path in the store that isn't a directory, which is unexpected: {}",
                entry.path().to_string_lossy()
            ));
        }
    }

    Ok(path_set)
}

#[tokio::main]
async fn async_main(args: Args) -> anyhow::Result<()> {
    let existing_store_paths = probe_nix_store(&args.nix_store_path).await?;

    let keychain = CachePublicKeychain::with_known_keys()?;

    let downloader = Downloader::new(
        args.temp_download_location,
        args.cache_url,
        args.cache_auth_token,
        existing_store_paths,
        keychain,
    );
    let downloader = downloader.start();

    let unpacker = Unpacker::new(args.nix_store_path.clone());
    let unpacker = unpacker.start();

    let downloader_data = web::Data::new(downloader.child());
    let unpacker_data = web::Data::new(unpacker.child());

    HttpServer::new(move || {
        App::new()
            .app_data(downloader_data.clone())
            .app_data(unpacker_data.clone())
            .route(
                "/",
                web::post()
                    .guard(fn_guard(|c| {
                        c.head().headers().get("content-length").is_some_and(|val| {
                            val.to_str()
                                .is_ok_and(|len| len.parse::<u32>().is_ok_and(|l| l > 0))
                        })
                    }))
                    .to(entry_point),
            )
            .route("/", web::to(HttpResponse::ImATeapot))
    })
    .shutdown_timeout(5)
    .bind(("0.0.0.0", args.port))?
    .run()
    .await?;

    Ok(())
}

// Main is not async because we need to make sure we deal with all the capabilities on the initial thread before we spawn any others.
fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    process_init::ensure_caps()?;
    process_init::prepare_nix_store(&args.nix_store_path)?;
    process_init::drop_caps()?;

    async_main(args)
}
