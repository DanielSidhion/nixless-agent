use std::{collections::HashSet, path::PathBuf};

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
}

pub enum DeleterRequest {
    DeletePackages {
        package_ids: HashSet<String>,
        resp_tx: oneshot::Sender<anyhow::Result<()>>,
    },
}

pub struct StartedDeleter {
    task: Option<JoinHandle<anyhow::Result<()>>>,
    input_tx: mpsc::Sender<DeleterRequest>,
}

impl StartedDeleter {
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

        let task = tokio::spawn(deleter_task(self.nix_store_dir, input_rx));

        StartedDeleter {
            task: Some(task),
            input_tx,
        }
    }
}

#[instrument(skip_all)]
async fn deleter_task(
    nix_store_dir: PathBuf,
    input_rx: mpsc::Receiver<DeleterRequest>,
) -> anyhow::Result<()> {
    let mut input_stream = ReceiverStream::new(input_rx);

    tracing::info!("Deleter will now enter its main loop.");

    while let Some(req) = input_stream.next().await {
        match req {
            DeleterRequest::DeletePackages {
                package_ids,
                resp_tx,
            } => {
                let nix_store_dir_clone = nix_store_dir.clone();
                // Enclosed in a new task so we can easily catch any errors.
                let delete_task = tokio::spawn(async move {
                    for package_id in package_ids {
                        let package_path = nix_store_dir_clone.join(package_id);

                        if !package_path.exists() {
                            continue;
                        }

                        remove_readonly_path(package_path).await?;
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

    Ok(())
}
