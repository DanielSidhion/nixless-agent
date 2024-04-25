use std::{
    collections::HashSet,
    ffi::OsStr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::anyhow;
use futures::future::join_all;
use nix::unistd::{chown, geteuid};
use tracing::instrument;

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

pub fn set_group_write_perm(path: impl AsRef<Path>) -> anyhow::Result<()> {
    let path = path.as_ref();

    let attr = std::fs::symlink_metadata(path)?;
    let mut permissions = attr.permissions();

    if permissions.mode() & 0o020 != 0o020 {
        permissions.set_mode(permissions.mode() | 0o020);
        std::fs::set_permissions(path, permissions)?;
    }

    Ok(())
}

#[tracing::instrument]
pub async fn remove_path(path: PathBuf) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }

    tracing::info!("Removing path!");

    if path.is_dir() {
        tokio::fs::remove_dir_all(&path).await?;
    } else {
        tokio::fs::remove_file(&path).await?;
    }

    Ok(())
}

pub async fn clean_up_nix_var_dir(base_dir: PathBuf) -> anyhow::Result<()> {
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
        .map(|&rp| base_dir.join(rp))
        .collect();

    let removal_futures: Vec<_> = paths_to_remove
        .drain(..)
        .map(|path| remove_path(path))
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

#[instrument(skip_all)]
pub async fn collect_nix_store_packages(
    store_dir: impl AsRef<Path>,
) -> anyhow::Result<HashSet<String>> {
    let store_dir_str = store_dir.as_ref().to_str().ok_or_else(|| {
        anyhow!("the store directory path couldn't be transformed to a utf-8 string")
    })?;

    let mut entries = tokio::fs::read_dir(&store_dir).await?;
    let mut package_id_set = HashSet::new();

    while let Some(entry) = entries.next_entry().await? {
        if let Some(path_str) = entry.path().to_str() {
            package_id_set.insert(
                path_str
                    .trim_start_matches(store_dir_str)
                    .trim_start_matches("/")
                    .to_string(),
            );
        } else {
            return Err(anyhow!("found a package in the store with a path containing non-UTF-8 characters, which is unexpected: {}", entry.path().to_string_lossy()));
        }
    }

    Ok(package_id_set)
}
