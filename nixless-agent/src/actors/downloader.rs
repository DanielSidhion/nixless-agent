use std::path::PathBuf;

use anyhow::{anyhow, Context};
use derive_builder::Builder;
use futures::StreamExt;
use narinfo::{NarInfo, NixCacheInfo};
use nix_core::{to_nix32, NixStylePublicKey, PublicKeychain};
use reqwest::header::{HeaderMap, HeaderValue};
use sha2::{Digest, Sha256};
use tokio::{
    fs::File,
    io::{AsyncWriteExt, BufWriter},
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::io::{InspectWriter, StreamReader};
use tracing::instrument;
use xz_decoder::XZDecoder;

use crate::{
    fingerprint::Fingerprint, owned_nar_info::OwnedNarInfo, path_utils::collect_nix_store_packages,
};

#[derive(Builder)]
pub struct Downloader {
    nix_store_dir: String,
    temp_download_path: PathBuf,
    cache_url: String,
    cache_auth_token: Option<String>,
    cache_public_key: Option<String>,
}

pub enum DownloaderRequest {
    DownloadPackages {
        package_ids: Vec<String>,
        resp_tx: oneshot::Sender<anyhow::Result<Vec<NarDownloadResult>>>,
    },
}

pub struct StartedDownloader {
    task: Option<JoinHandle<anyhow::Result<()>>>,
    input_tx: mpsc::Sender<DownloaderRequest>,
}

impl StartedDownloader {
    pub fn child(&self) -> Self {
        Self {
            task: None,
            input_tx: self.input_tx.clone(),
        }
    }

    pub async fn shutdown(self) -> anyhow::Result<()> {
        if let Some(task) = self.task {
            task.await??;
        }

        Ok(())
    }

    pub async fn download_packages(
        &self,
        package_ids: Vec<String>,
    ) -> anyhow::Result<Vec<NarDownloadResult>> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.input_tx
            .send(DownloaderRequest::DownloadPackages {
                package_ids,
                resp_tx,
            })
            .await?;

        resp_rx.await?
    }
}

impl Downloader {
    pub fn builder() -> DownloaderBuilder {
        DownloaderBuilder::default()
    }

    pub fn start(self) -> StartedDownloader {
        let (input_tx, input_rx) = mpsc::channel(10);

        let task = tokio::spawn(async {
            match downloader_task(
                self.nix_store_dir,
                self.temp_download_path,
                self.cache_url,
                self.cache_auth_token,
                self.cache_public_key,
                input_rx,
            )
            .await
            {
                Ok(()) => Ok(()),
                Err(err) => {
                    tracing::error!(
                        ?err,
                        "The downloader task encountered a fatal error and has stopped."
                    );
                    Err(err)
                }
            }
        });

        StartedDownloader {
            task: Some(task),
            input_tx,
        }
    }
}

#[instrument(skip_all)]
async fn downloader_task(
    nix_store_dir: String,
    temp_download_path: PathBuf,
    cache_url: String,
    cache_auth_token: Option<String>,
    cache_public_key: Option<String>,
    input_rx: mpsc::Receiver<DownloaderRequest>,
) -> anyhow::Result<()> {
    let mut keychain = PublicKeychain::with_known_keys()?;

    if let Some(cache_public_key) = cache_public_key {
        tracing::info!(
            cache_public_key,
            "Adding the configured public key of the binary cache as a trusted key."
        );

        keychain.add_key(NixStylePublicKey::from_nix_format(&cache_public_key)?)?;
    }

    tracing::info!(
        nix_store_dir,
        "Reading the nix store to determine all existing packages."
    );

    let mut existing_store_package_ids = collect_nix_store_packages(&nix_store_dir).await?;

    tracing::info!(
        nix_store_dir,
        "Finished reading the nix store to determine all existing packages."
    );

    let mut default_headers = HeaderMap::new();

    if let Some(token) = cache_auth_token {
        let mut header_value = HeaderValue::from_str(&format!("bearer {}", token))?;
        header_value.set_sensitive(true);
        default_headers.insert("authorization", header_value);
    }

    let client = reqwest::Client::builder()
        .default_headers(default_headers)
        .build()?;

    tracing::debug!(
        cache_url,
        "Verifying if the configured binary cache has a matching store path."
    );

    // Before we start doing any work, we should check if the cache given to us has the same store path as us. If it doesn't, it's unlikely that the packages we retrieve will work on our machine.
    let resp = client
        .get(format!("{}/nix-cache-info", cache_url))
        .header("accept", "text/plain")
        .send()
        .await
        // TODO: also send a signal to the rest of the application?
        .context("failed to verify if the cache has the same store path as us")?;

    if resp.status().is_success() {
        let resp_text = resp.text().await?;
        let nix_cache_info = NixCacheInfo::parse(&resp_text)
            .map_err(|parsing_error| anyhow!("{:#?}", parsing_error))?;

        if nix_cache_info.store_dir != nix_store_dir {
            // TODO: re-enable this.
            // return Err(anyhow!(
            //     "Cache has a store path different from ours. Got {}, expected {}",
            //     nix_cache_info.store_dir,
            //     store_path
            // ));
        } else {
            tracing::debug!("Cache store path matches ours! Continuing.");
        }
    } else {
        return Err(anyhow!(
            "Cache returned a {} when trying to verify its store path!",
            resp.status().as_str()
        ));
    }

    tracing::info!("Downloader has finished initialisation and will now enter its main loop.");

    let mut input_stream = ReceiverStream::new(input_rx);

    while let Some(req) = input_stream.next().await {
        match req {
            DownloaderRequest::DownloadPackages {
                package_ids,
                resp_tx,
            } => {
                let mut download_futures = Vec::new();
                let mut existing_package_ids = Vec::new();

                for package_id in package_ids {
                    if existing_store_package_ids.contains(&package_id) {
                        existing_package_ids.push(package_id);
                        continue;
                    }

                    download_futures.push(download_one_nar(
                        client.clone(),
                        &temp_download_path,
                        &cache_url,
                        package_id,
                        &keychain,
                    ));
                }

                tracing::info!(
                    locally_owned = existing_package_ids.len(),
                    to_download = download_futures.len(),
                    "Started task to download any missing packages."
                );

                let download_futures = futures::stream::iter(download_futures);
                // We need to collect from the stream into a Vec of Results first, because the stream doesn't allow us to directly convert from a Vec of Results into a Result of Vec.
                // TODO: make number of parallel downloads configurable.
                let mut download_results: Result<Vec<_>, _> = download_futures
                    .buffer_unordered(5)
                    .collect::<Vec<_>>()
                    .await
                    .into_iter()
                    .collect();

                tracing::info!("Finished downloading all missing packages.");

                // We'll augment the download results with the store packages we already had. This is not that efficient because we have to re-fetch the NAR info for the packages we already have.
                // TODO: make this code more efficient by saving the references for the NARs we download locally so we can load them here instead of fetching from the binary cache again. The references can be cleaned up once the system download operation completes and we verify all references are downloaded.
                if let Ok(ref mut curr_download_results) = download_results {
                    tracing::info!(
                        "Augmenting download results with all packages we already had locally."
                    );

                    for existing_package_id in existing_package_ids {
                        let nar_info =
                            download_nar_info(&client, &cache_url, &existing_package_id).await?;
                        curr_download_results.push(NarDownloadResult {
                            package_id: existing_package_id,
                            nar_path: temp_download_path.join(nar_info.url),
                            reference_ids: nar_info.references,
                            is_already_unpacked: true,
                        });
                    }
                }

                let resp = match download_results {
                    Ok(download_results) => {
                        // If we're here, it means no download returned an error, so we'll assume every store path will be populated once the NARs are unpacked. With this assumption, we'll already extend our set of existing store paths. If there's an error eventually when unpacking the NARs, the system will be in an inconsistent state and it's expected that it will take the proper action to bring consistency back.
                        download_results.iter().for_each(|r| {
                            existing_store_package_ids.insert(r.package_id.clone());
                        });

                        // We'll check that all references for the NARs we downloaded exist (or will exist) locally, otherwise we'll have to error to prevent the system from pointing to a path that doesn't exist.
                        if download_results.iter().any(|r| {
                            r.reference_ids
                                .iter()
                                .any(|rp| !existing_store_package_ids.contains(rp))
                        }) {
                            Err(anyhow!(
                                "the paths that were downloaded have missing references!"
                            ))
                        } else {
                            Ok(download_results)
                        }
                    }
                    err => err,
                };

                resp_tx.send(resp).map_err(|_| {
                    anyhow!("the channel got closed before we could send a message to it!")
                })?;
            }
        }
    }

    Ok(())
}

pub struct NarDownloadResult {
    pub package_id: String,
    pub nar_path: PathBuf,
    pub reference_ids: Vec<String>,
    pub is_already_unpacked: bool,
}

async fn download_one_nar(
    client: reqwest::Client,
    download_dir: &PathBuf,
    cache_url: &str,
    package_id: String,
    keychain: &PublicKeychain,
) -> anyhow::Result<NarDownloadResult> {
    let nar_info = download_nar_info(&client, cache_url, &package_id).await?;

    let nar_hash_parts: Vec<_> = nar_info.nar_hash.split(":").collect();
    let ["sha256", nar_hash] = nar_hash_parts[..] else {
        return Err(anyhow!(
            "The NAR hash doesn't follow the format we expected. Got {}, expected sha256:<hash>",
            nar_info.nar_hash
        ));
    };

    let file_hash = if let Some(file_hash_inner) = nar_info.file_hash.as_ref() {
        let file_hash_parts: Vec<_> = file_hash_inner.split(":").collect();
        let ["sha256", hash] = file_hash_parts[..] else {
            return Err(anyhow!("The file hash doesn't follow the format we expected. Got {}, expected sha256:<hash>",
            nar_info.nar_hash));
        };
        hash
    } else {
        ""
    };

    if !nar_info.verify_fingerprint(keychain)? {
        return Err(anyhow!(
            "Couldn't verify the signature of the NAR we downloaded!"
        ));
    }

    // TODO: as an optimisation, if the NAR file already exists in the download location, check if its hash matches what we got. If it does, we can skip downloading entirely.

    let nardata_url = format!("{}/{}", cache_url, nar_info.url);
    let mut local_nar_path = download_dir.join(nar_info.url);

    // In case any of the parent directories don't exist, we create them.
    std::fs::create_dir_all(local_nar_path.parent().unwrap())?;

    let resp = client
        .get(nardata_url)
        .header("accept", "application/x-nix-nar")
        .send()
        .await?;

    if resp.status().is_success() {
        let mut stream_reader = StreamReader::new(resp.bytes_stream().map(|result| {
            result.map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))
        }));

        // TODO: deal with multiple compression options for the NAR. Remember when "Compression: none" exists.

        if let Some(ext) = local_nar_path.extension() {
            if ext == "xz" {
                local_nar_path = local_nar_path.with_extension("");
            }
        }
        // We'll craft the following pipeline: (response body) -> (compressed hasher) -> (xz decoder) -> (decompressed hasher) -> (file writer) -> (file).
        let file = File::options()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&local_nar_path)
            .await?;

        let file_writer = BufWriter::new(file);

        let mut decompressed_hasher = Sha256::new();
        let decompressed_inspector = InspectWriter::new(file_writer, |chunk| {
            decompressed_hasher.update(chunk);
        });

        let decompresser = if let Some(compression_type) = &nar_info.compression {
            match compression_type.as_str() {
                "none" => tokio_util::either::Either::Right(BufWriter::new(decompressed_inspector)),
                "xz" => tokio_util::either::Either::Left(XZDecoder::new(decompressed_inspector)?),
                _ => todo!("other compression types not yet implemented"),
            }
        } else {
            todo!("nar info without compression not yet implemented");
        };

        // TODO: In case we don't have a `file_hash`, it would be a good idea to skip doing the hashing here, but the code got somewhat complicated and would need a bit of care to get right.
        let mut compressed_hasher = Sha256::new();
        let mut compressed_inspector = InspectWriter::new(decompresser, |chunk| {
            compressed_hasher.update(chunk);
        });

        tokio::io::copy(&mut stream_reader, &mut compressed_inspector).await?;
        compressed_inspector.flush().await?;

        let decompressed_hash = to_nix32(&decompressed_hasher.finalize());
        if decompressed_hash != nar_hash {
            return Err(anyhow!(
                "the hash of the decompressed NAR doesn't match. Got {}, expected {}",
                decompressed_hash,
                nar_hash
            ));
        }

        if file_hash != "" {
            let compressed_hash = to_nix32(&compressed_hasher.finalize());
            if compressed_hash != file_hash {
                return Err(anyhow!(
                    "the hash of the compressed NAR doesn't match. Got {}, expected {}",
                    compressed_hash,
                    file_hash
                ));
            }
        }

        Ok(NarDownloadResult {
            package_id,
            nar_path: local_nar_path,
            reference_ids: nar_info
                .references
                .into_iter()
                .filter_map(|r| {
                    let text = r.trim();
                    if text.is_empty() {
                        None
                    } else {
                        Some(text.to_string())
                    }
                })
                .collect(),
            is_already_unpacked: false,
        })
    } else {
        Err(anyhow!(
            "trying to fetch {} returned a {} status code",
            local_nar_path.to_string_lossy(),
            resp.status().as_str()
        ))
    }
}

async fn download_nar_info(
    client: &reqwest::Client,
    cache_url: &str,
    package_id: &str,
) -> anyhow::Result<OwnedNarInfo> {
    let narinfo_url: String;

    if let Some((hash, _name)) = package_id.split_once("-") {
        narinfo_url = format!("{}/{}.narinfo", cache_url, hash);
    } else {
        return Err(anyhow!(
            "Received an unexpected package id to download: {}",
            package_id
        ));
    }

    // Protocol as seen in https://github.com/fzakaria/nix-http-binary-cache-api-spec
    let resp = client
        .get(narinfo_url)
        .header("accept", "text/x-nix-narinfo")
        .send()
        .await?;

    let nar_info_text: String;

    if resp.status().is_success() {
        nar_info_text = resp.text().await?;
    } else {
        return Err(anyhow!(
            "Got a bad response from the cache server! {}",
            resp.status().as_str()
        ));
    }

    let nar_info =
        NarInfo::parse(&nar_info_text).map_err(|parsing_error| anyhow!("{:#?}", parsing_error))?;

    if !nar_info.store_path.ends_with(&package_id) {
        return Err(anyhow!(
            "The info from the cache points to a different package. Expected it to end with {}, got {}",
            package_id,
            nar_info.store_path
        ));
    }

    Ok(nar_info.into())
}
