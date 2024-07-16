use std::{
    fs::{read_dir, File},
    iter::repeat_with,
    ops::Deref,
    os::unix::fs::lchown,
    path::PathBuf,
    time::SystemTime,
};

use anyhow::{anyhow, Context};
use derive_builder::Builder;
use nix::sys::{
    stat::{utimensat, UtimensatFlags},
    time::TimeSpec,
};
use nix_nar::Decoder;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tracing::instrument;

use super::NarDownloadResult;

#[derive(Builder)]
pub struct Unpacker {
    nix_store_dir: PathBuf,
}

pub enum UnpackerRequest {
    UnpackDownloads {
        downloads: Vec<NarDownloadResult>,
        resp_tx: oneshot::Sender<anyhow::Result<()>>,
    },
    Shutdown,
}

#[derive(Debug)]
pub struct StartedUnpacker {
    task: JoinHandle<anyhow::Result<()>>,
    input: StartedUnpackerInput,
}

impl StartedUnpacker {
    pub fn input(&self) -> StartedUnpackerInput {
        self.input.clone()
    }

    pub async fn shutdown(self) -> anyhow::Result<()> {
        self.input.input_tx.send(UnpackerRequest::Shutdown).await?;
        self.task.await?
    }
}

impl Deref for StartedUnpacker {
    type Target = StartedUnpackerInput;

    fn deref(&self) -> &Self::Target {
        &self.input
    }
}

#[derive(Clone, Debug)]
pub struct StartedUnpackerInput {
    input_tx: mpsc::Sender<UnpackerRequest>,
}

impl StartedUnpackerInput {
    pub async fn unpack_downloads(&self, downloads: Vec<NarDownloadResult>) -> anyhow::Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.input_tx
            .send(UnpackerRequest::UnpackDownloads { downloads, resp_tx })
            .await?;

        resp_rx.await?
    }
}

impl Unpacker {
    pub fn builder() -> UnpackerBuilder {
        UnpackerBuilder::default()
    }

    pub fn start(self) -> StartedUnpacker {
        let (input_tx, input_rx) = mpsc::channel(10);

        let task = tokio::spawn(unpacker_task(self.nix_store_dir, input_rx));

        StartedUnpacker {
            task,
            input: StartedUnpackerInput { input_tx },
        }
    }
}

#[instrument(skip_all)]
async fn unpacker_task(
    nix_store_dir: PathBuf,
    input_rx: mpsc::Receiver<UnpackerRequest>,
) -> anyhow::Result<()> {
    let mut input_stream = ReceiverStream::new(input_rx);

    tracing::info!("Unpacker will now enter its main loop.");

    while let Some(req) = input_stream.next().await {
        match req {
            UnpackerRequest::Shutdown => {
                tracing::info!("Unpacker got a request to shutdown. Shutting down.");
                break;
            }
            UnpackerRequest::UnpackDownloads { downloads, resp_tx } => {
                // TODO: this currently runs on a single thread. Moving it to multiple threads (but still bounded by some limit) is not too trivial and will require a bit of thought.
                let nix_store_dir_clone = nix_store_dir.clone();
                let unpack_task = tokio::task::spawn_blocking(move || {
                    let downloads_to_unpack =
                        downloads.into_iter().filter(|d| !d.is_already_unpacked);
                    for download in downloads_to_unpack {
                        unpack_one_nar(
                            &nix_store_dir_clone,
                            &download.package_id,
                            &download.nar_path,
                        )?;
                    }

                    Ok(())
                });

                let res = unpack_task.await?;
                resp_tx
                    .send(res)
                    .map_err(|_| anyhow!("channel closed before we could send the response"))?;
            }
        }
    }

    Ok(())
}

fn unpack_one_nar(
    nix_store_dir: &PathBuf,
    package_id: &str,
    nar_path: &PathBuf,
) -> anyhow::Result<()> {
    // TODO: double check that the NAR exists and the store path to unpack to doesn't exist.

    let tmp_dir_name: String = repeat_with(fastrand::alphanumeric).take(12).collect();
    let tmp_dir = nix_store_dir.join(tmp_dir_name);

    let file = File::options().read(true).open(nar_path)?;
    let nar_decoder = Decoder::new(file)?;
    nar_decoder
        .unpack(&tmp_dir)
        .context("Failed to unpack a NAR with the decoder")?;
    drop(nar_decoder);

    let final_path = nix_store_dir.join(package_id);

    std::fs::rename(&tmp_dir, &final_path)?;
    finalise_nix_store_object(&final_path)?;

    // Since the NAR unpacking is done, we'll delete it.
    std::fs::remove_file(nar_path)?;

    Ok(())
}

/// Objects in the Nix store shouldn't be writable, their timestamps should be set to the epoch, certain attributes removed and so on. This function handles all of that.
/// Note that here we use "object" to mean not only a package in the Nix store, but also each file/directory/symlink inside the package. We call each one of those an "object".
// TODO: check if more stuff needs to be done from https://github.com/NixOS/nix/blob/9b88e5284608116b7db0dbd3d5dd7a33b90d52d7/src/libstore/posix-fs-canonicalise.cc#L58
fn finalise_nix_store_object(object_path: &PathBuf) -> anyhow::Result<()> {
    let stat = std::fs::symlink_metadata(object_path)?;

    if !stat.is_symlink() {
        let mut perms = stat.permissions();
        if !perms.readonly() {
            // Fix permissions. Can't have writable stuff in the store.
            perms.set_readonly(true);
            std::fs::set_permissions(object_path, perms)?;
        }
    }

    if stat
        .modified()?
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_millis()
        != 1000
    {
        // Fix modified time, which should be 1 second after the epoch.
        utimensat(
            None,
            object_path,
            &TimeSpec::UTIME_OMIT,
            &TimeSpec::new(1, 0),
            UtimensatFlags::NoFollowSymlink,
        )?;
    }

    if stat.is_dir() {
        // Before changing the owner, we'll recurse in the directory fixing all other permissions first, and change the owner from the bottom-up to prevent getting locked out from making any other changes.
        for entry in read_dir(object_path)? {
            finalise_nix_store_object(&entry?.path())?;
        }
    }

    lchown(object_path, Some(0), Some(0))?;
    Ok(())
}
