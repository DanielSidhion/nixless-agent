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
    DownloadingNewConfiguration {
        configuration: SystemConfiguration,
    },
    SwitchingToConfiguration {
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
            | Self::DownloadingNewConfiguration { configuration }
            | Self::SwitchingToConfiguration { configuration } => Some(configuration),
            Self::Temporary => unreachable!("Temporary agent status shouldn't be reachable"),
        }
    }

    pub fn inner_configuration_system_package_id(&self) -> Option<String> {
        match self {
            Self::New | Self::Standby => None,
            Self::FailedSwitch { configuration }
            | Self::DownloadingNewConfiguration { configuration }
            | Self::SwitchingToConfiguration { configuration } => {
                Some(configuration.system_package_id.clone())
            }
            Self::Temporary => unreachable!("Temporary agent status shouldn't be reachable"),
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct AgentState {
    #[serde(skip)]
    nix_store_dir: String,
    #[serde(skip)]
    nix_state_base_dir: PathBuf,
    #[serde(skip)]
    nixless_state_dir: PathBuf,
    #[serde(skip)]
    state_file_path: PathBuf,

    system_configurations: Vec<SystemConfiguration>,
    current_status: AgentStateStatus,
}

impl AgentState {
    fn relative_state_path() -> &'static str {
        "state"
    }

    fn current_system_path() -> &'static str {
        "/run/current-system"
    }

    fn relative_system_profile_path() -> &'static str {
        "nix/profiles/system"
    }

    pub fn absolute_state_path(&self) -> PathBuf {
        self.nixless_state_dir.join(Self::relative_state_path())
    }

    /// This ends with `_associated` just because we have a method with the same name, so the `_associated` disambiguates to show that this is an associated function rather than a method.
    fn absolute_state_path_associated(nixless_state_dir: &PathBuf) -> PathBuf {
        nixless_state_dir.join(Self::relative_state_path())
    }

    fn absolute_system_profile_path(&self) -> PathBuf {
        self.nix_state_base_dir
            .join(Self::relative_system_profile_path())
    }

    fn absolute_profiles_dir(&self) -> PathBuf {
        self.nix_state_base_dir.join("nix/profiles")
    }

    fn absolute_numbered_system_profile_path(&self, num: u32) -> PathBuf {
        self.nix_state_base_dir
            .join(format!("nix/profiles/system-{}-link", num))
    }

    pub async fn from_saved_state_or_new(
        nix_store_dir: String,
        nix_state_base_dir: PathBuf,
        nixless_state_dir: PathBuf,
    ) -> anyhow::Result<Self> {
        let state_file_path = Self::absolute_state_path_associated(&nixless_state_dir);

        if !state_file_path.exists() {
            Self::new(
                nix_store_dir,
                nix_state_base_dir,
                nixless_state_dir,
                state_file_path,
            )
            .await
        } else {
            let mut state: Self =
                serde_json::from_str(&tokio::fs::read_to_string(&state_file_path).await?)?;

            state.nix_store_dir = nix_store_dir;
            state.nix_state_base_dir = nix_state_base_dir;
            state.nixless_state_dir = nixless_state_dir;
            state.state_file_path = state_file_path;
            Ok(state)
        }
    }

    /// Tries to determine the current configuration by inspecting the current system path, which is usually at `/run/current-system`.
    async fn new(
        nix_store_dir: String,
        nix_state_base_dir: PathBuf,
        nixless_state_dir: PathBuf,
        state_file_path: PathBuf,
    ) -> anyhow::Result<Self> {
        // Will be used if we can't determine the configuration of the current system.
        let build_tombstone_value = || -> anyhow::Result<SystemConfiguration> {
            Ok(SystemConfiguration::builder()
                .version_number(0)
                .system_package_id("unknown".to_string())
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
            Ok(current_system_package_path) => {
                // We don't want to throw an error if we can't convert it to a utf-8 string, we'll just use the tombstone value instead.
                if let Some(current_system_package_path) = current_system_package_path.to_str() {
                    // We have the package id, but also must figure out the number it corresponds to. Since we can't do this from the current system path, we'll try to get it by inspecting the current system profile.
                    let current_version_number = Self::get_current_numbered_system_number(
                        &nix_state_base_dir,
                        current_system_package_path,
                    )
                    .await
                    .unwrap_or(0);

                    SystemConfiguration::builder()
                        .version_number(current_version_number)
                        .system_package_id(
                            current_system_package_path
                                .trim_start_matches(&nix_store_dir)
                                .trim_start_matches("/")
                                .to_string(),
                        )
                        .build()?
                } else {
                    build_tombstone_value()?
                }
            }
        };

        Ok(Self {
            nix_store_dir,
            nix_state_base_dir,
            nixless_state_dir,
            state_file_path,
            system_configurations: vec![current_configuration],
            current_status: AgentStateStatus::New,
        })
    }

    pub fn base_dir(&self) -> PathBuf {
        self.nixless_state_dir.clone()
    }

    pub fn base_dir_nix(&self) -> PathBuf {
        self.nix_state_base_dir.clone()
    }

    pub fn status(&self) -> &AgentStateStatus {
        &self.current_status
    }

    pub fn set_standby(&mut self) -> anyhow::Result<()> {
        self.current_status = AgentStateStatus::Standby;
        self.save()
    }

    pub fn new_configuration_system_package_path(&self) -> Option<String> {
        self.current_status.inner_configuration_system_package_id()
    }

    fn latest_configuration_version(&self) -> u32 {
        self.system_configurations
            .last()
            .map(|c| c.version_number)
            .unwrap()
    }

    fn latest_system_package_path(&self) -> PathBuf {
        let mut p = PathBuf::from_str(&self.nix_store_dir).unwrap();
        p.push(&self.system_configurations.last().unwrap().system_package_id);
        p
    }

    async fn ensure_profiles_directory_exists(&self) -> anyhow::Result<()> {
        let profiles_dir_path = self.absolute_profiles_dir();

        if !profiles_dir_path.exists() {
            tokio::fs::create_dir_all(&profiles_dir_path).await?;
        }

        Ok(())
    }

    async fn repair_profile_links(&mut self) -> anyhow::Result<()> {
        self.ensure_profiles_directory_exists().await?;

        // We'll first clean up any numbered system links that we're not tracking.
        let mut dir_entries = tokio::fs::read_dir(self.absolute_profiles_dir()).await?;

        while let Some(entry) = dir_entries.next_entry().await? {
            if entry.file_name() == "system" {
                continue;
            }

            let entry_number = get_number_from_numbered_system_name(&entry.file_name())?;

            if self
                .system_configurations
                .iter()
                .all(|v| v.version_number != entry_number)
            {
                // The current entry is not for the current system, or a numbered version that we know of, so we'll remove it.
                tokio::fs::remove_file(entry.path()).await?;
            }
        }

        // And then recreate any missing numbered system links that we're tracking. In case this code is called just after a successful system switch, this part ensures that we'll create a `system-<num>-link` link for the new configuration.
        for config in self.system_configurations.iter() {
            let expected_profile_path =
                self.absolute_numbered_system_profile_path(config.version_number);

            let full_system_package_path = {
                let mut p = PathBuf::from_str(&self.nix_store_dir)?;
                p.push(&config.system_package_id);
                p
            };

            overwrite_symlink_atomically_with_check(
                full_system_package_path,
                &expected_profile_path,
            )
            .await?;
        }

        // Lastly, we ensure that the `system` symlink points to the latest `system-<num>-link` we're tracking.
        overwrite_symlink_atomically_with_check(
            self.latest_system_package_path(),
            &self.absolute_system_profile_path(),
        )
        .await?;

        Ok(())
    }

    pub async fn mark_new_system_successful(&mut self) -> anyhow::Result<()> {
        if let AgentStateStatus::SwitchingToConfiguration { .. } = &self.current_status {
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
        if let AgentStateStatus::SwitchingToConfiguration { .. } = &self.current_status {
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

    pub fn mark_switching_new_system(
        &mut self,
        system_package_id: String,
        package_ids: Vec<String>,
    ) -> anyhow::Result<()> {
        if !matches!(self.current_status, AgentStateStatus::Standby) {
            return Err(anyhow!(
                "current state is not standby, we can't switch to a new system"
            ));
        }

        let next_version_number = self.latest_configuration_version() + 1;

        let new_configuration = SystemConfiguration::builder()
            .version_number(next_version_number)
            .system_package_id(system_package_id)
            .package_ids(package_ids)
            .build()?;

        self.current_status = AgentStateStatus::SwitchingToConfiguration {
            configuration: new_configuration,
        };

        Ok(())
    }

    fn save(&self) -> anyhow::Result<()> {
        let mut file = std::fs::File::options()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.state_file_path)?;
        serde_json::to_writer(&mut file, self)?;
        Ok(())
    }

    async fn get_current_numbered_system_number(
        nix_state_base_dir: &PathBuf,
        current_system_package_path: &str,
    ) -> anyhow::Result<u32> {
        // Will get us only the `system-<num>-link` part. We assume that's the format, and then process it to get the `<num>` part only.
        let current_numbered_system_path =
            tokio::fs::read_link(nix_state_base_dir.join(Self::relative_system_profile_path()))
                .await?;
        let current_version_number: u32 = get_number_from_numbered_system_name(
            current_numbered_system_path.file_name().unwrap(),
        )?;

        // As a sanity check, we'll make sure this numbered path points to the same system as the one we were given.
        let current_actual_system_path =
            tokio::fs::read_link(&current_numbered_system_path).await?;

        if current_actual_system_path.to_str().ok_or_else(|| {
            anyhow!("the current numbered system points to a path that can't be converted to utf-8")
        })? != current_system_package_path
        {
            Err(anyhow!(
                "the current numbered system points to a different system than we were given"
            ))
        } else {
            Ok(current_version_number)
        }
    }
}
