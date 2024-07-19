use std::{collections::HashSet, ops::Deref, path::PathBuf};

use anyhow::anyhow;
use derive_builder::Builder;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tracing::instrument;

use crate::path_utils::remove_readonly_path;

#[derive(Builder)]
pub struct Deleter {
    nix_store_dir: PathBuf,
    nar_info_cache_dir: PathBuf,
}

pub enum DeleterRequest {
    DeletePackages {
        package_ids: HashSet<String>,
        resp_tx: oneshot::Sender<anyhow::Result<()>>,
    },
    Shutdown,
}

#[derive(Debug)]
pub struct StartedDeleter {
    task: JoinHandle<anyhow::Result<()>>,
    input: StartedDeleterInput,
}

#[derive(Clone, Debug)]
pub struct StartedDeleterInput {
    input_tx: mpsc::Sender<DeleterRequest>,
}

impl StartedDeleter {
    pub fn input(&self) -> StartedDeleterInput {
        self.input.clone()
    }

    pub async fn shutdown(self) -> anyhow::Result<()> {
        self.input.input_tx.send(DeleterRequest::Shutdown).await?;
        self.task.await?
    }
}

impl Deref for StartedDeleter {
    type Target = StartedDeleterInput;

    fn deref(&self) -> &Self::Target {
        &self.input
    }
}

impl StartedDeleterInput {
    pub async fn delete_packages(&self, package_ids: HashSet<String>) -> anyhow::Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.input_tx
            .send(DeleterRequest::DeletePackages {
                package_ids,
                resp_tx,
            })
            .await?;

        resp_rx.await?
    }
}

impl Deleter {
    pub fn builder() -> DeleterBuilder {
        DeleterBuilder::default()
    }

    pub fn start(self) -> StartedDeleter {
        let (input_tx, input_rx) = mpsc::channel(10);

        let task = tokio::spawn(deleter_task(
            self.nix_store_dir,
            self.nar_info_cache_dir,
            input_rx,
        ));

        StartedDeleter {
            task,
            input: StartedDeleterInput { input_tx },
        }
    }
}

#[instrument(skip_all)]
async fn deleter_task(
    nix_store_dir: PathBuf,
    nar_info_cache_dir: PathBuf,
    input_rx: mpsc::Receiver<DeleterRequest>,
) -> anyhow::Result<()> {
    let mut input_stream = ReceiverStream::new(input_rx);

    tracing::info!("Deleter will now enter its main loop.");

    while let Some(req) = input_stream.next().await {
        match req {
            DeleterRequest::Shutdown => {
                tracing::info!("Deleter got a request to shutdown. Proceeding.");
                break;
            }
            DeleterRequest::DeletePackages {
                package_ids,
                resp_tx,
            } => {
                let nix_store_dir_clone = nix_store_dir.clone();
                let nar_info_cache_dir_clone = nar_info_cache_dir.clone();
                // Enclosed in a new task so we can easily catch any errors.
                let delete_task = tokio::spawn(async move {
                    for package_id in package_ids {
                        let package_path = nix_store_dir_clone.join(&package_id);

                        if !package_path.exists() {
                            continue;
                        }

                        let cached_nar_info_path = package_id
                            .split_once("-")
                            .map(|(hash, _name)| nar_info_cache_dir_clone.join(hash))
                            .filter(|p| p.exists());

                        if let Some(cached_nar_info_path) = cached_nar_info_path {
                            let res = tokio::join!(
                                remove_readonly_path(package_path),
                                remove_readonly_path(cached_nar_info_path)
                            );
                            [res.0, res.1].into_iter().collect::<Result<_, _>>()?;
                        } else {
                            remove_readonly_path(package_path).await?;
                        }
                    }

                    Ok(())
                });

                let res = delete_task.await?;
                resp_tx
                    .send(res)
                    .map_err(|_| anyhow!("channel closed before we could send the response"))?;
            }
        }
    }

    tracing::info!("Deleter has finished shutting down.");
    Ok(())
}
