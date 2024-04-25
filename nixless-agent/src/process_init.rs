use std::{
    fs::{read_dir, read_link, read_to_string},
    path::{Path, PathBuf},
};

use anyhow::anyhow;
use caps::{CapSet, Capability};
use nix::{
    mount::{mount, MsFlags},
    sched::{unshare, CloneFlags},
    sys::statvfs::{statvfs, FsFlags},
    unistd::{chown, getegid, Gid},
};

use crate::path_utils::set_group_write_perm;

pub fn ensure_caps() -> anyhow::Result<()> {
    let mut effective_set = caps::read(None, CapSet::Effective)?;
    let mut should_raise = false;

    if !effective_set.contains(&Capability::CAP_SETPCAP) {
        effective_set.insert(Capability::CAP_SETPCAP);
        should_raise = true;
    }
    if !effective_set.contains(&Capability::CAP_SYS_ADMIN) {
        effective_set.insert(Capability::CAP_SYS_ADMIN);
        should_raise = true;
    }
    if !effective_set.contains(&Capability::CAP_CHOWN) {
        effective_set.insert(Capability::CAP_CHOWN);
        should_raise = true;
    }
    if !effective_set.contains(&Capability::CAP_FOWNER) {
        effective_set.insert(Capability::CAP_FOWNER);
        should_raise = true;
    }

    if should_raise {
        caps::set(None, CapSet::Effective, &effective_set)?;
    }

    Ok(())
}

// Adapted from https://github.com/NixOS/nix/blob/845b2a9256bd1541abbe66b3129c87713983d073/src/libstore/local-store.cc#L574
pub fn prepare_nix_store(store_path: &PathBuf) -> anyhow::Result<()> {
    let stat = statvfs(store_path)?;

    if stat.flags().contains(FsFlags::ST_RDONLY) {
        // The read-only mount to prevent changes to the Nix store exists, so we'll get rid of the mount by moving into a different mount namespace and remounting the store. This will ensure only this process has write access to the Nix store.
        unshare(CloneFlags::CLONE_NEWNS)?;
        mount(
            None::<&PathBuf>,
            store_path,
            None::<&str>,
            MsFlags::MS_BIND | MsFlags::MS_REMOUNT,
            None::<&str>,
        )?;
    }

    let current_gid = getegid();
    // Allows us to unpack NARs into the store.
    chown(store_path, None, Some(current_gid))?;

    Ok(())
}

pub fn prepare_nix_state(state_path: &PathBuf) -> anyhow::Result<()> {
    let current_gid = getegid();

    // We'll start with the parent of the nix state (which should be `/nix`) so we can have permissions to make the `/nix/var` dir and its descendants writable - we'll add and remove stuff in there.
    let parent = state_path
        .parent()
        .ok_or_else(|| anyhow!("the nix state path doesn't have a parent"))?;
    chown(parent, None, Some(current_gid.clone()))?;
    set_group_write_perm(parent)?;

    prepare_nix_state_dir(state_path, &current_gid)?;
    Ok(())
}

fn prepare_nix_state_dir(curr_dir_path: &Path, gid: &Gid) -> anyhow::Result<()> {
    chown(curr_dir_path, None, Some(*gid))?;
    set_group_write_perm(curr_dir_path)?;

    for entry in read_dir(curr_dir_path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            prepare_nix_state_dir(&entry.path(), gid)?;
        }
    }

    Ok(())
}

pub fn drop_caps() -> anyhow::Result<()> {
    // We'll still need CAP_CHOWN when unpacking NARs into the store, but the other caps can go away.
    // TODO: optimise this into fewer calls.
    caps::clear(None, CapSet::Ambient)?;
    caps::clear(None, CapSet::Inheritable)?;
    caps::drop(None, CapSet::Effective, Capability::CAP_SYS_ADMIN)?;
    caps::drop(None, CapSet::Effective, Capability::CAP_SETPCAP)?;
    caps::drop(None, CapSet::Effective, Capability::CAP_FOWNER)?;
    caps::drop(None, CapSet::Permitted, Capability::CAP_SYS_ADMIN)?;
    caps::drop(None, CapSet::Permitted, Capability::CAP_SETPCAP)?;
    caps::drop(None, CapSet::Permitted, Capability::CAP_FOWNER)?;
    Ok(())
}

/// This can never guarantee the nix daemon isn't running in the system, but for all common cases, it will catch the daemon and error when it does.
pub fn ensure_nix_daemon_not_present() -> anyhow::Result<()> {
    let procs = read_dir("/proc")?;

    for entry in procs {
        let entry = entry?;
        let entry_file_name = entry.file_name();
        let file_name = entry_file_name
            .to_str()
            .ok_or_else(|| anyhow!("Entry in /proc has a path that can't be converted to UTF-8"))?;

        if file_name.parse::<usize>().is_ok() && entry.file_type()?.is_dir() {
            // We'll first check the link to the executable, and if that fails we'll check the cmdline.
            let path = entry.path();
            let exe_path = path.join("exe");

            match read_link(&exe_path) {
                Ok(exe_link)
                    if exe_link
                        .file_name()
                        .is_some_and(|n| n == "nix" || n == "nix-daemon") =>
                {
                    return Err(anyhow!(
                        "Detected pid {} running the nix binary, so we can't start",
                        file_name
                    ));
                }
                Ok(_) => (),
                Err(_) => {
                    let cmdline_contents = read_to_string(path.join("cmdline"))?;

                    if cmdline_contents.contains("nix-daemon") {
                        return Err(anyhow!(
                            "Detected pid {} running the nix binary, so we can't start",
                            file_name
                        ));
                    }
                }
            };
        }
    }

    Ok(())
}

pub fn load_extra_env_file() -> anyhow::Result<()> {
    let env_file_path = match ::std::env::var("NIXLESS_AGENT_EXTRA_ENV_FILE") {
        Ok(val) => PathBuf::from(val),
        Err(_) => {
            let systemd_state_directory =
                ::std::env::var("STATE_DIRECTORY").unwrap_or_else(|_| String::new());

            let mut dot_env_path = PathBuf::from(&systemd_state_directory);
            dot_env_path.push(".env");
            dot_env_path
        }
    };

    tracing::info!(?env_file_path, "Loading additional environment variables.");

    dotenvy::from_path(env_file_path).or_else(|e| match e {
        dotenvy::Error::Io(io_error)
            if matches!(io_error.kind(), ::std::io::ErrorKind::NotFound) =>
        {
            // If we don't find any .env files to load, just keep going instead of erroring out.
            Ok(())
        }
        other => Err(other),
    })?;

    Ok(())
}
