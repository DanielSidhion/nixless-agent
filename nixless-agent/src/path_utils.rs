use std::{
    ffi::OsStr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::anyhow;
use futures::future::join_all;
use nix::unistd::{chown, geteuid};

pub fn get_number_from_numbered_system_name(name: &OsStr) -> anyhow::Result<u32> {
    Ok(name
        .to_str()
        .ok_or_else(|| {
            anyhow!("the current numbered system link can't be converted to an UTF-8 string")
        })?
        .split("-")
        .nth(1)
        .ok_or_else(|| {
            anyhow!("the current numbered system link doesn't follow the format we expected")
        })?
        .parse()?)
}

pub async fn overwrite_symlink_atomically_with_check(
    target: impl AsRef<Path>,
    symlink_path: &PathBuf,
) -> anyhow::Result<()> {
    if symlink_path.exists() {
        let current_target = tokio::fs::read_link(symlink_path).await?;

        if current_target == target.as_ref() {
            return Ok(());
        }
    }

    overwrite_symlink_atomically(target, symlink_path).await
}

pub async fn overwrite_symlink_atomically(
    target: impl AsRef<Path>,
    symlink_path: &PathBuf,
) -> anyhow::Result<()> {
    let mut temporary_symlink_path = symlink_path.clone();
    let mut temporary_symlink_name = temporary_symlink_path.file_name().unwrap().to_os_string();
    // TODO: perhaps use a more randomised suffix to avoid accidentally using a temporary name that already exists.
    temporary_symlink_name.push("-temporary");
    temporary_symlink_path.set_file_name(temporary_symlink_name);

    tokio::fs::symlink(target, &temporary_symlink_path).await?;
    tokio::fs::rename(temporary_symlink_path, symlink_path).await?;

    Ok(())
}

// Usually, the paths are owned by root, so we can't directly delete them. The way we'll do that is by changing the ownership to us, ensuring we can write to the directory/file, and then remove it.
pub async fn chown_and_remove(path: PathBuf) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }

    let current_uid = geteuid();

    chown(&path, Some(current_uid), None)?;
    let attr = tokio::fs::symlink_metadata(&path).await?;
    let mut permissions = attr.permissions();

    if permissions.mode() & 0o200 != 0o200 {
        permissions.set_mode(permissions.mode() | 0o200);
        tokio::fs::set_permissions(&path, permissions).await?;
    }

    if attr.is_dir() {
        tokio::fs::remove_dir_all(&path).await?;
    } else {
        tokio::fs::remove_file(&path).await?;
    }

    Ok(())
}

pub async fn clean_up_nix_dir(dir: PathBuf) -> anyhow::Result<()> {
    let relative_paths_to_remove = &[
        "log",
        "nix/daemon-socket",
        "nix/db",
        "nix/gc.lock",
        "nix/gcroots",
        "nix/gc-socket",
        "nix/temproots",
        "nix/userpool",
        "nix/profiles/per-user",
        "nix/profiles/default",
    ];
    let mut paths_to_remove: Vec<_> = relative_paths_to_remove
        .iter()
        .map(|&rp| dir.join(rp))
        .collect();

    let removal_futures: Vec<_> = paths_to_remove
        .drain(..)
        .map(|path| chown_and_remove(path))
        .collect();

    join_all(removal_futures)
        .await
        .into_iter()
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(())
}

pub async fn remove_file_with_check(path: impl AsRef<Path>) -> anyhow::Result<()> {
    if tokio::fs::try_exists(path.as_ref()).await? {
        tokio::fs::remove_file(path.as_ref()).await?;
    }

    Ok(())
}