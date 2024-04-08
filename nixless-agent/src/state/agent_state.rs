use std::{path::PathBuf, str::FromStr};

use anyhow::anyhow;
use serde::{Deserialize, Serialize};

use crate::{
    path_utils::{get_number_from_numbered_system_name, overwrite_symlink_atomically_with_check},
    system_configuration::SystemConfiguration,
};

#[derive(Deserialize, Serialize)]
pub enum AgentStateStatus {
    New,
    Standby,
    FailedSwitch {
        configuration: SystemConfiguration,
    },
    DownloadingNewSystem {
        configuration: SystemConfiguration,
    },
    SwitchingToNewSystem {
        configuration: SystemConfiguration,
    },
    /// Only used as a temporary variant to avoid copying/cloning the SystemConfiguration of other variants. The agent state should never be left at this value.
    Temporary,
}

impl AgentStateStatus {
    pub fn into_inner_configuration(self) -> Option<SystemConfiguration> {
        match self {
            Self::New | Self::Standby => None,
            Self::FailedSwitch { configuration }
            | Self::DownloadingNewSystem { configuration }
            | Self::SwitchingToNewSystem { configuration } => Some(configuration),
            Self::Temporary => unreachable!("Temporary agent status shouldn't be reachable"),
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct AgentState {
    #[serde(skip)]
    directory: PathBuf,
    #[serde(skip)]
    state_file_path: PathBuf,

    system_configurations: Vec<SystemConfiguration>,
    current_status: AgentStateStatus,
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

    pub async fn from_directory(directory: PathBuf) -> anyhow::Result<Self> {
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
        // Will be used if we can't determine the top level of the current system.
        let build_tombstone_value = || -> anyhow::Result<SystemConfiguration> {
            Ok(SystemConfiguration::builder()
                .version_number(0)
                .toplevel_path_string("unknown".to_string())
                .build()?)
        };

        let current_configuration = match tokio::fs::canonicalize(Self::current_system_path()).await
        {
            Err(_) => build_tombstone_value()?,
            Ok(current_version_path)
                if !current_version_path.exists() || !current_version_path.is_dir() =>
            {
                build_tombstone_value()?
            }
            Ok(current_version_path) => {
                // We don't want to throw an error if we can't convert it to a utf-8 string, we'll just use the tombstone value instead.
                if let Some(path_string) = current_version_path.to_str() {
                    let current_version_number = Self::get_current_system_numbered_path(&directory)
                        .await
                        .unwrap_or(0);

                    SystemConfiguration::builder()
                        .version_number(current_version_number)
                        .toplevel_path_string(path_string.to_string())
                        .build()?
                } else {
                    build_tombstone_value()?
                }
            }
        };

        Ok(Self {
            directory,
            state_file_path,
            system_configurations: vec![current_configuration],
            current_status: AgentStateStatus::New,
        })
    }

    pub fn status(&self) -> &AgentStateStatus {
        &self.current_status
    }

    pub fn set_standby(&mut self) -> anyhow::Result<()> {
        self.current_status = AgentStateStatus::Standby;
        self.save()
    }

    fn latest_system_toplevel_path_string(&self) -> &String {
        &self
            .system_configurations
            .last()
            .unwrap()
            .toplevel_path_string
    }

    async fn repair_profile_links(&mut self) -> anyhow::Result<()> {
        // We'll first clean up any numbered system links that we're not tracking.
        let mut dir_entries = tokio::fs::read_dir(self.absolute_profiles_directory_path()).await?;

        while let Some(entry) = dir_entries.next_entry().await? {
            let entry_number = get_number_from_numbered_system_name(&entry.file_name())?;

            if entry.file_name() == "system"
                || self
                    .system_configurations
                    .iter()
                    .any(|v| v.version_number == entry_number)
            {
                continue;
            }

            // The current entry is not for the system link, or a numbered version that we know of, so we'll remove it.
            tokio::fs::remove_file(entry.path()).await?;
        }

        // And then recreate any missing numbered system links that we're tracking. In case this code is called just after a successful system switch, this part ensures that we'll create a `system-<num>-link` link for the new configuration.
        for config in self.system_configurations.iter() {
            let expected_profile_path =
                self.absolute_numbered_system_profile_path(config.version_number);

            overwrite_symlink_atomically_with_check(
                PathBuf::from_str(&config.toplevel_path_string)?,
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

    pub async fn mark_new_system_successful(&mut self) -> anyhow::Result<()> {
        if let AgentStateStatus::SwitchingToNewSystem { .. } = &self.current_status {
            let previous_status =
                std::mem::replace(&mut self.current_status, AgentStateStatus::Standby);
            self.system_configurations
                .push(previous_status.into_inner_configuration().unwrap());
            self.save()?;
            // Will take care of fixing the links to the system profile for us.
            self.repair_profile_links().await?;

            Ok(())
        } else {
            Err(anyhow!("we're not switching to a new system at the moment"))
        }
    }

    pub async fn mark_new_system_failed(&mut self) -> anyhow::Result<()> {
        if let AgentStateStatus::SwitchingToNewSystem { .. } = &self.current_status {
            let previous_status =
                std::mem::replace(&mut self.current_status, AgentStateStatus::Temporary);
            self.current_status = AgentStateStatus::FailedSwitch {
                configuration: previous_status.into_inner_configuration().unwrap(),
            };
            self.save()?;

            Ok(())
        } else {
            Err(anyhow!("we're not switching to a new system at the moment"))
        }
    }

    fn save(&self) -> anyhow::Result<()> {
        let parent_dir = self.state_file_path.parent().unwrap();

        if !parent_dir.exists() {
            std::fs::create_dir_all(parent_dir)?;
        }

        let mut file = std::fs::File::options()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.state_file_path)?;
        serde_json::to_writer(&mut file, self)?;
        Ok(())
    }

    async fn get_current_system_numbered_path(directory: &PathBuf) -> anyhow::Result<u32> {
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
