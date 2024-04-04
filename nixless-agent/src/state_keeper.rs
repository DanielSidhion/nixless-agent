use std::{
    ffi::OsStr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::anyhow;
use futures::future::join_all;
use nix::unistd::{chown, geteuid};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

use crate::downloader::DownloaderRequest;

pub struct StateKeeper {
    directory: PathBuf,
}

#[derive(Deserialize, Serialize)]
pub struct AgentState {
    #[serde(skip)]
    directory: PathBuf,
    #[serde(skip)]
    state_file_path: PathBuf,

    system_versions: Vec<(u32, String)>,
    current_status: AgentStateStatus,
}

fn get_number_from_numbered_system_name(name: &OsStr) -> anyhow::Result<u32> {
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

async fn overwrite_symlink_atomically_with_check(
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

async fn overwrite_symlink_atomically(
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

#[derive(Deserialize, Serialize)]
pub enum AgentStateStatus {
    New,
    Standby,
    FailedSwitch { system_id: String },
    DownloadingNewSystem { system_id: String },
    SwitchingToNewSystem { system_id: String },
}

impl AgentState {
    fn relative_state_path() -> &'static str {
        "nixless-agent/state"
    }

    fn current_system_path() -> &'static str {
        "/run/current-system"
    }

    fn relative_system_profile_path() -> &'static str {
        "nix/profiles/system"
    }

    fn absolute_system_profile_path(&self) -> PathBuf {
        self.directory.join(Self::relative_system_profile_path())
    }

    fn absolute_profiles_directory_path(&self) -> PathBuf {
        self.directory.join("nix/profiles")
    }

    fn absolute_numbered_system_profile_path(&self, num: u32) -> PathBuf {
        self.directory
            .join(format!("nix/profiles/system-{}-link", num))
    }

    async fn from_directory(directory: PathBuf) -> anyhow::Result<Self> {
        let state_file_path = directory.join(Self::relative_state_path());

        if !state_file_path.exists() {
            Self::new(directory, state_file_path).await
        } else {
            let mut state: Self =
                serde_json::from_str(&tokio::fs::read_to_string(&state_file_path).await?)?;

            state.directory = directory;
            state.state_file_path = state_file_path;
            Ok(state)
        }
    }

    async fn new(directory: PathBuf, state_file_path: PathBuf) -> anyhow::Result<Self> {
        // TODO: first check if the current system path exists, otherwise add a marker in the system versions so we know to avoid cleaning up too many systems.
        let current_version_path = tokio::fs::canonicalize(Self::current_system_path()).await?;
        if !current_version_path.exists() || !current_version_path.is_dir() {
            return Err(anyhow!("can't determine the current system configuration because /run/current-system doesn't point to an existing directory"));
        }
        let current_version_path_string = current_version_path.to_str().ok_or_else(|| anyhow!("the current system configuration is in a path that can't be converted to an UTF-8 string"))?.to_string();

        let current_version_number = Self::get_current_system_numbered_path(&directory).await?;

        Ok(Self {
            directory,
            state_file_path,
            system_versions: vec![(current_version_number, current_version_path_string)],
            current_status: AgentStateStatus::New,
        })
    }

    fn status(&self) -> &AgentStateStatus {
        &self.current_status
    }

    fn set_standby(&mut self) {
        self.current_status = AgentStateStatus::Standby;
    }

    fn latest_version_number(&self) -> u32 {
        self.system_versions.last().unwrap().0
    }

    fn latest_system_toplevel_path_string(&self) -> &String {
        &self.system_versions.last().unwrap().1
    }

    async fn repair_profile_links(&mut self) -> anyhow::Result<()> {
        // We'll first clean up any numbered system links that we're not tracking.
        let mut dir_entries = tokio::fs::read_dir(self.absolute_profiles_directory_path()).await?;

        while let Some(entry) = dir_entries.next_entry().await? {
            let entry_number = get_number_from_numbered_system_name(&entry.file_name())?;

            if entry.file_name() == "system"
                || self.system_versions.iter().any(|v| v.0 == entry_number)
            {
                continue;
            }

            // The current entry is not for the system link, or a numbered version that we know of, so we'll remove it.
            tokio::fs::remove_file(entry.path()).await?;
        }

        // And then recreate any missing numbered system links that we're tracking. In case this code is called just after a successful system switch, this part ensures that we'll create a `system-<num>-link` link for the new configuration.
        for (version, toplevel_path) in self.system_versions.iter() {
            let expected_profile_path = self.absolute_numbered_system_profile_path(*version);

            overwrite_symlink_atomically_with_check(
                PathBuf::from_str(&toplevel_path)?,
                &expected_profile_path,
            )
            .await?;
        }

        // Lastly, we ensure that the `system` symlink points to the latest `system-<num>-link` we're tracking.
        overwrite_symlink_atomically_with_check(
            PathBuf::from(self.latest_system_toplevel_path_string()),
            &self.absolute_system_profile_path(),
        )
        .await?;

        Ok(())
    }

    async fn mark_new_system_successful(&mut self) -> anyhow::Result<()> {
        if let AgentStateStatus::SwitchingToNewSystem { system_id } = &self.current_status {
            let next_version_number = self.latest_version_number() + 1;
            self.system_versions
                .push((next_version_number, system_id.clone()));
            self.current_status = AgentStateStatus::Standby;
            self.save().await?;
            // Will take care of fixing the links to the system profile for us.
            self.repair_profile_links().await?;

            Ok(())
        } else {
            Err(anyhow!("we're not switching to a new system at the moment"))
        }
    }

    fn mark_new_system_failed(&mut self) -> anyhow::Result<()> {
        if let AgentStateStatus::SwitchingToNewSystem { system_id } = &self.current_status {
            self.current_status = AgentStateStatus::FailedSwitch {
                system_id: system_id.clone(),
            };

            Ok(())
        } else {
            Err(anyhow!("we're not switching to a new system at the moment"))
        }
    }

    async fn save(&self) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(self)?;
        Ok(tokio::fs::write(&self.state_file_path, bytes).await?)
    }

    async fn get_current_system_numbered_path(directory: &PathBuf) -> anyhow::Result<u32> {
        // TODO: first check if the path exists.
        let current_system_numbered_path =
            tokio::fs::read_link(directory.join(Self::relative_system_profile_path())).await?;
        let current_version_number: u32 = get_number_from_numbered_system_name(
            current_system_numbered_path
                // Will get us only the `system-<num>-link` part. We assume that's the format, and then process it to get the `<num>` only.
                .file_name()
                .unwrap(),
        )?;

        Ok(current_version_number)
    }
}

impl StateKeeper {
    pub fn with_state_directory(mut directory: PathBuf) -> anyhow::Result<Self> {
        Ok(Self { directory })
    }

    pub fn start(self) -> (StartedStateKeeper, ControlChannels) {
        let (input_tx, input_rx) = mpsc::channel(10);
        let (external_channels, internal_channels) = new_control_channels();

        let task = tokio::spawn(state_keeper_task(
            self.directory,
            internal_channels,
            input_rx,
            input_tx.clone(),
        ));

        (
            StartedStateKeeper {
                task: Some(task),
                input_tx,
            },
            external_channels,
        )
    }
}

struct ControlChannelsInternal {
    startup_continue_tx: oneshot::Sender<()>,
    downloader_input_tx: mpsc::Sender<DownloaderRequest>,
}

pub struct ControlChannels {
    startup_continue_rx: Option<oneshot::Receiver<()>>,
    downloader_input_rx: Option<mpsc::Receiver<DownloaderRequest>>,
}

fn new_control_channels() -> (ControlChannels, ControlChannelsInternal) {
    let (startup_continue_tx, startup_continue_rx) = oneshot::channel();
    let (downloader_input_tx, downloader_input_rx) = mpsc::channel(10);

    let external = ControlChannels {
        startup_continue_rx: Some(startup_continue_rx),
        downloader_input_rx: Some(downloader_input_rx),
    };
    let internal = ControlChannelsInternal {
        startup_continue_tx,
        downloader_input_tx,
    };

    (external, internal)
}

pub enum StateKeeperRequest {
    CleanUpDir,
}

pub struct StartedStateKeeper {
    task: Option<JoinHandle<anyhow::Result<()>>>,
    input_tx: mpsc::Sender<StateKeeperRequest>,
}

impl StartedStateKeeper {
    pub fn child(&self) -> Self {
        Self {
            task: None,
            input_tx: self.input_tx.clone(),
        }
    }
}

async fn state_keeper_task(
    directory: PathBuf,
    control_channels: ControlChannelsInternal,
    input_rx: mpsc::Receiver<StateKeeperRequest>,
    input_tx: mpsc::Sender<StateKeeperRequest>,
) -> anyhow::Result<()> {
    let mut state = AgentState::from_directory(directory.clone()).await?;

    // If we're here, we just got started, so we'll check what was our previous status and figure out next steps from there.
    match state.status() {
        AgentStateStatus::New | AgentStateStatus::Standby => {
            // We can start operating normally, but we'll enqueue a job to clean up the directory.
            state.set_standby();
            input_tx.send(StateKeeperRequest::CleanUpDir).await?;
        }
        AgentStateStatus::FailedSwitch { .. } => {
            // We'll start in a "read-only" mode.
        }
        AgentStateStatus::DownloadingNewSystem { system_id } => {
            // We'll continue downloading the new system, but aside from that will operate normally.
            control_channels
                .downloader_input_tx
                .send(DownloaderRequest::DownloadSystem {
                    system_id: system_id.clone(),
                })
                .await?;
        }
        AgentStateStatus::SwitchingToNewSystem { .. } => {
            // We must check whether we switched successfully or not.
            let switching_status = check_switching_status(&directory).await?;
            match (
                switching_status.started,
                switching_status.finished,
                switching_status.successful,
            ) {
                (true, true, true) => {
                    // TODO: check if we have to reboot due to a kernel or some other thing that changed that requires a reboot. If we do, only consider things successful after the reboot.
                    clean_up_tracking_files(&directory).await?;
                    state.mark_new_system_successful().await?;
                }
                (true, true, false) => {
                    let status_codes = switching_status.status_codes.unwrap();
                    if status_codes.service_result == "exit-code"
                        && status_codes.exit_status == "100"
                    {
                        // Switch was "successful", but requires a reboot. Only consider things successful after the reboot.
                    } else {
                        // We failed for real. We're in an inconsistent state, so we'll start in a "read-only" mode.
                        state.mark_new_system_failed()?;
                    }
                }
                (_, false, _) | (false, _, _) => {
                    // TODO: wait until finished
                }
            }
        }
    }

    // At this point, we're ready to proceed with normal operations, so we'll signal to the rest of the system that startup can continue.
    control_channels.startup_continue_tx.send(()).map_err(|_| {
        anyhow!("the startup continue channel dropped before we even signaled it to continue")
    })?;

    Ok(())
}

async fn clean_up_dir(dir: PathBuf) -> anyhow::Result<()> {
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

// Usually, the paths are owned by root, so we can't directly delete them. The way we'll do that is by changing the ownership to us, ensuring we can write to the directory/file, and then remove it.
async fn chown_and_remove(path: PathBuf) -> anyhow::Result<()> {
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

struct SystemSwitchStatus {
    started: bool,
    finished: bool,
    successful: bool,

    status_codes: Option<SwitchStatusCodes>,
}

async fn clean_up_tracking_files(directory: &PathBuf) -> anyhow::Result<()> {
    let started_path = directory.join("nixless-agent/pre_switch");
    let success_path = directory.join("nixless-agent/switch_success");
    let finish_path = directory.join("nixless-agent/post_switch");

    let (r1, r2, r3) = tokio::join!(
        remove_file_with_check(started_path),
        remove_file_with_check(success_path),
        remove_file_with_check(finish_path)
    );
    r1?;
    r2?;
    r3?;

    Ok(())
}

async fn remove_file_with_check(path: impl AsRef<Path>) -> anyhow::Result<()> {
    if tokio::fs::try_exists(path.as_ref()).await? {
        tokio::fs::remove_file(path.as_ref()).await?;
    }

    Ok(())
}

async fn check_switching_status(directory: &PathBuf) -> anyhow::Result<SystemSwitchStatus> {
    let started_path = directory.join("nixless-agent/pre_switch");
    let success_path = directory.join("nixless-agent/switch_success");
    let finish_path = directory.join("nixless-agent/post_switch");

    let finished = finish_path.try_exists()?;
    let status_codes = if finished {
        let contents = tokio::fs::read_to_string(finish_path).await?;
        let [service_result, exit_code, exit_status] = contents.lines().collect::<Vec<_>>()[..]
        else {
            return Err(anyhow!(
                "the tracking file for finished status didn't follow the expected format"
            ));
        };

        Some(SwitchStatusCodes {
            service_result: service_result.to_string(),
            exit_code: exit_code.to_string(),
            exit_status: exit_status.to_string(),
        })
    } else {
        None
    };

    Ok(SystemSwitchStatus {
        started: started_path.try_exists()?,
        finished,
        successful: success_path.try_exists()?,
        status_codes,
    })
}

struct SwitchStatusCodes {
    service_result: String,
    exit_code: String,
    exit_status: String,
}
