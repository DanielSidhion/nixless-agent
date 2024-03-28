use std::{
    fs::{read_dir, File},
    iter::repeat_with,
    os::unix::fs::{chown, lchown},
    path::PathBuf,
    time::SystemTime,
};

use anyhow::anyhow;
use nix::sys::{
    stat::{lstat, utimensat, UtimensatFlags},
    time::TimeSpec,
};
use nix_nar::Decoder;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

use crate::downloader::NarDownloadResult;

pub struct Unpacker {
    store_path: PathBuf,
}

pub enum UnpackerRequest {
    UnpackDownloads {
        downloads: Vec<NarDownloadResult>,
        resp_tx: oneshot::Sender<anyhow::Result<()>>,
    },
}

pub struct StartedUnpacker {
    task: Option<JoinHandle<anyhow::Result<()>>>,
    input_tx: mpsc::Sender<UnpackerRequest>,
}

impl StartedUnpacker {
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

    pub async fn unpack_downloads(&self, downloads: Vec<NarDownloadResult>) -> anyhow::Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.input_tx
            .send(UnpackerRequest::UnpackDownloads { downloads, resp_tx })
            .await?;

        resp_rx.await?
    }
}

impl Unpacker {
    pub fn new(store_path: PathBuf) -> Self {
        Self { store_path }
    }

    pub fn start(self) -> StartedUnpacker {
        let (input_tx, input_rx) = mpsc::channel(10);

        let task = tokio::spawn(unpacker_task(self.store_path, input_rx));

        StartedUnpacker {
            task: Some(task),
            input_tx,
        }
    }
}

async fn unpacker_task(
    store_path: PathBuf,
    mut input_rx: mpsc::Receiver<UnpackerRequest>,
) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            req = input_rx.recv() => {
                match req {
                    None => break,
                    Some(UnpackerRequest::UnpackDownloads { downloads, resp_tx }) => {
                        println!("Got downloads to unpack!");

                        // TODO: this currently runs on a single thread. Moving it to multiple threads (but still bounded by some limit) is not too trivial and will require a bit of thought.
                        let store_path_copy = store_path.clone();
                        let unpack_task = tokio::task::spawn_blocking(move || {
                            for download in downloads {
                                unpack_one_nar(&store_path_copy, &download.store_path, &download.nar_path)?;
                            }

                            Ok(())
                        });

                        let res = unpack_task.await?;
                        resp_tx.send(res).map_err(|_| anyhow!("channel closed before we could send the response"))?;
                    }
                }
            }
        }
    }

    Ok(())
}

fn unpack_one_nar(
    nix_store_path: &PathBuf,
    store_path: &str,
    nar_path: &PathBuf,
) -> anyhow::Result<()> {
    let tmp_dir_name: String = repeat_with(fastrand::alphanumeric).take(12).collect();
    let tmp_dir = nix_store_path.join(tmp_dir_name);

    let file = File::options().read(true).open(nar_path)?;
    let nar_decoder = Decoder::new(file)?;
    nar_decoder.unpack(&tmp_dir)?;
    drop(nar_decoder);

    let final_path = nix_store_path.join(store_path);

    std::fs::rename(&tmp_dir, &final_path)?;
    finalise_store_path(&final_path)?;

    // Since the NAR unpacking is done, we'll delete it.
    std::fs::remove_file(nar_path)?;

    Ok(())
}

/// Nix store objects shouldn't be writable, their timestamps should be set to the epoch, certain attributes removed and so on. This function handles all of that.
// TODO: check if more stuff needs to be done from https://github.com/NixOS/nix/blob/9b88e5284608116b7db0dbd3d5dd7a33b90d52d7/src/libstore/posix-fs-canonicalise.cc#L58
fn finalise_store_path(path: &PathBuf) -> anyhow::Result<()> {
    let stat = std::fs::symlink_metadata(path)?;

    if !stat.is_symlink() {
        let mut perms = stat.permissions();
        if !perms.readonly() {
            // Fix permissions. Can't have writable stuff in the store.
            perms.set_readonly(true);
            std::fs::set_permissions(path, perms)?;
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
            path,
            &TimeSpec::UTIME_OMIT,
            &TimeSpec::new(1, 0),
            UtimensatFlags::NoFollowSymlink,
        )?;
    }

    if stat.is_dir() {
        // Before changing the owner, we'll recurse in the directory fixing all other permissions first, and change the owner from the bottom-up to prevent getting locked out from making any other changes.
        for entry in read_dir(path)? {
            finalise_store_path(&entry?.path())?;
        }
    }

    lchown(path, Some(0), Some(0))?;
    Ok(())
}
