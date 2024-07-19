use std::{collections::HashSet, ops::Deref, sync::Arc};

use anyhow::anyhow;
use derive_builder::Builder;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tracing::instrument;

use crate::{
    dbus_connection::StartedDBusConnection,
    metrics,
    path_utils::clean_up_nix_var_dir,
    state::{
        calculate_switch_duration, check_switching_status, record_switch_start, AgentState,
        AgentStateStatus, SystemSummary, SystemSwitchStatus,
    },
};

use super::{StartedDeleter, StartedDownloader, StartedUnpacker};

#[derive(Builder)]
#[builder(pattern = "owned")]
pub struct StateKeeper {
    state: AgentState,
    dbus_connection: StartedDBusConnection,
    downloader: StartedDownloader,
    unpacker: StartedUnpacker,
    deleter: StartedDeleter,
}

impl StateKeeper {
    pub fn builder() -> StateKeeperBuilder {
        StateKeeperBuilder::default()
    }

    pub fn start(self) -> StartedStateKeeper {
        let (input_tx, input_rx) = mpsc::channel(10);

        let input_tx_clone = input_tx.clone();
        let task = tokio::spawn(async {
            match state_keeper_task(
                self.state,
                self.dbus_connection,
                self.downloader,
                self.unpacker,
                self.deleter,
                input_rx,
                input_tx_clone,
            )
            .await
            {
                Ok(()) => Ok(()),
                Err(err) => {
                    tracing::error!(
                        ?err,
                        "The state keeper task encountered a fatal error and has stopped."
                    );
                    Err(err)
                }
            }
        });

        StartedStateKeeper {
            task,
            input: StartedStateKeeperInput { input_tx },
        }
    }
}

// TODO: add a message to sweep the nix store dir and check for any foreign packages.
enum StateKeeperRequest {
    CleanUpStateDir,
    CleanUpStateDirResult(anyhow::Result<()>),
    SwitchToNewConfiguration {
        system_package_id: String,
        package_ids: HashSet<String>,
        resp_tx: oneshot::Sender<anyhow::Result<()>>,
    },
    ConfigurationSwitchStartResult(anyhow::Result<()>),
    CleanupConfigurationHistory,
    PackageDeletionResult(anyhow::Result<()>),
    GetSummary {
        resp_tx: oneshot::Sender<anyhow::Result<SystemSummary>>,
    },
    PerformRollback {
        to_version: Option<u32>,
        resp_tx: oneshot::Sender<anyhow::Result<()>>,
    },
    Shutdown,
}

#[derive(Debug)]
pub struct StartedStateKeeper {
    task: JoinHandle<anyhow::Result<()>>,
    input: StartedStateKeeperInput,
}

impl Deref for StartedStateKeeper {
    type Target = StartedStateKeeperInput;

    fn deref(&self) -> &Self::Target {
        &self.input
    }
}

impl StartedStateKeeper {
    pub fn input(&self) -> StartedStateKeeperInput {
        self.input.clone()
    }

    pub async fn shutdown(self) -> anyhow::Result<()> {
        self.input
            .input_tx
            .send(StateKeeperRequest::Shutdown)
            .await?;
        self.task.await?
    }
}

#[derive(Clone, Debug)]
pub struct StartedStateKeeperInput {
    input_tx: mpsc::Sender<StateKeeperRequest>,
}

impl StartedStateKeeperInput {
    pub async fn switch_to_new_configuration(
        &self,
        system_package_id: String,
        package_ids: HashSet<String>,
    ) -> anyhow::Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.input_tx
            .send(StateKeeperRequest::SwitchToNewConfiguration {
                system_package_id,
                package_ids,
                resp_tx,
            })
            .await?;

        resp_rx.await?
    }

    pub async fn get_summary(&self) -> anyhow::Result<SystemSummary> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.input_tx
            .send(StateKeeperRequest::GetSummary { resp_tx })
            .await?;

        resp_rx.await?
    }

    pub async fn perform_rollback(&self, to_version: Option<u32>) -> anyhow::Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.input_tx
            .send(StateKeeperRequest::PerformRollback {
                to_version,
                resp_tx,
            })
            .await?;

        resp_rx.await?
    }
}

#[instrument(skip_all)]
async fn state_keeper_task(
    mut state: AgentState,
    dbus_connection: StartedDBusConnection,
    downloader: StartedDownloader,
    unpacker: StartedUnpacker,
    deleter: StartedDeleter,
    input_rx: mpsc::Receiver<StateKeeperRequest>,
    input_tx: mpsc::Sender<StateKeeperRequest>,
) -> anyhow::Result<()> {
    tracing::info!("Checking if we can possibly be authorised to manage systemd units.");

    if !dbus_connection.check_authorisation_possibility().await? {
        return Err(anyhow!(
            "we're not authorised to manage systemd units, so we won't be able to switch systems"
        ));
    }

    tracing::info!("We might be authorised to manage systemd units, continuing initialisation.");

    let mut input_stream = ReceiverStream::new(input_rx);

    // If we're here, we just got started, so we'll check what was our previous status and figure out next steps from there.
    match state.status() {
        AgentStateStatus::Temporary => unreachable!("Temporary agent status should be unreachable"),
        AgentStateStatus::New | AgentStateStatus::Standby => {
            // We can start operating normally, but we'll enqueue a job to clean up the state directory.
            state.set_standby()?;
            input_tx.send(StateKeeperRequest::CleanUpStateDir).await?;
        }
        AgentStateStatus::FailedSwitch { .. } => {
            // We'll start in a "read-only" mode.
        }
        AgentStateStatus::DownloadingNewConfiguration { configuration } => {
            // We'll continue downloading the new system, but aside from that will operate normally.
            downloader
                .download_packages(configuration.package_ids.clone())
                .await?;
        }
        AgentStateStatus::SwitchingToConfiguration { .. } => {
            input_tx
                .send(StateKeeperRequest::ConfigurationSwitchStartResult(Ok(())))
                .await
                .unwrap();
        }
    }

    tracing::info!("State keeper finished early status decision-making, will now enter its main processing loop.");

    let mut pending_clean_up_task: Option<JoinHandle<()>> = None;
    let mut pending_system_switch_task: Option<JoinHandle<()>> = None;
    let mut pending_package_delete_task: Option<JoinHandle<()>> = None;

    while let Some(req) = input_stream.next().await {
        match req {
            StateKeeperRequest::Shutdown => {
                tracing::info!("State keeper got a request to shut down. Shutting down.");
                break;
            }
            StateKeeperRequest::CleanUpStateDir => {
                let input_tx_clone = input_tx.clone();
                let dir = state.base_dir_nix();
                tracing::info!("Starting a task to clean up the Nix state dir.");
                pending_clean_up_task = Some(tokio::spawn(async move {
                    let res = clean_up_nix_var_dir(dir).await;
                    input_tx_clone
                        .send(StateKeeperRequest::CleanUpStateDirResult(res))
                        .await
                        .unwrap();
                }));
            }
            StateKeeperRequest::CleanUpStateDirResult(Err(err)) => {
                tracing::warn!(?err, "We failed to clean up the state directory!");
                pending_clean_up_task = None;
            }
            StateKeeperRequest::CleanUpStateDirResult(Ok(())) => {
                tracing::info!("Task to clean up the Nix state dir succeeded!");
                pending_clean_up_task = None;
            }
            StateKeeperRequest::PerformRollback {
                to_version,
                resp_tx,
            } => {
                tracing::info!(
                    ?to_version,
                    "State keeper got a request to rollback configuration."
                );

                match state.status() {
                    AgentStateStatus::New | AgentStateStatus::Temporary => unreachable!("should have never been in a new or temporary state during the state keeper main loop"),
                    AgentStateStatus::DownloadingNewConfiguration { .. } => {
                        resp_tx.send(Err(anyhow!("The system is already downloading a new system configuration."))).map_err(|_| anyhow!("channel closed before we could send the response"))?;
                    }
                    AgentStateStatus::SwitchingToConfiguration { .. } => {
                        resp_tx.send(Err(anyhow!("The system is already switching to a new system configuration."))).map_err(|_| anyhow!("channel closed before we could send the response"))?;
                    }
                    AgentStateStatus::FailedSwitch { .. } | AgentStateStatus::Standby => {
                        state.mark_performing_rollback(to_version).await?;

                        let input_tx_clone = input_tx.clone();
                        let dbus_connection_input = dbus_connection.input();
                        // A bit annoying that we have to grab this from agent state, but seems like the better option. There are other ways to structure the code here to allow moving this stuff all inside the agent state so we don't need to clone the agent state or make an Arc or whatever, but I think this is fine for now.
                        let switch_start_file_path = state.absolute_switch_start_time_path();
                        let new_configuration_path = state.new_configuration_system_package_path().unwrap(); // We just marked that we're switching to a new system, so the `unwrap()` should never fail.
                        // We send the response just before starting the task just to try to avoid as much as possible any issues with never sending a response back if the system switch is almost immediate.
                        // TODO: guarantee that we'll wait until a response is sent back all the way through the server before we proceed with system switch?
                        resp_tx.send(Ok(())).map_err(|_| anyhow!("channel closed before we could send the response"))?;
                        pending_system_switch_task = Some(tokio::spawn(async move {
                            record_switch_start(switch_start_file_path.clone()).unwrap();
                            match dbus_connection_input.perform_configuration_switch(new_configuration_path).await {
                                Ok(()) => (),
                                Err(err) => {
                                    tracing::error!(?err, "Got an error when performing a system switch for a rollback.");
                                    input_tx_clone.send(StateKeeperRequest::ConfigurationSwitchStartResult(Err(err))).await.unwrap();
                                    return;
                                }
                            }

                            // We'll check if system switch was made successfully inside the state keeper code instead of this ad-hoc task.
                            input_tx_clone.send(StateKeeperRequest::ConfigurationSwitchStartResult(Ok(()))).await.unwrap();
                        }));
                    }
                }
            }
            StateKeeperRequest::SwitchToNewConfiguration {
                system_package_id,
                package_ids,
                resp_tx,
            } => {
                tracing::info!(
                    system_package_id,
                    "State keeper got a request to switch to new configuration."
                );

                match state.status() {
                    AgentStateStatus::New | AgentStateStatus::Temporary => unreachable!("should have never been in a new or temporary state during the state keeper main loop"),
                    AgentStateStatus::FailedSwitch { .. } => {
                        resp_tx.send(Err(anyhow!("The system already failed a system switch and must be recovered before switching to a new configuration."))).map_err(|_| anyhow!("channel closed before we could send the response"))?;
                    }
                    AgentStateStatus::DownloadingNewConfiguration { .. } => {
                        resp_tx.send(Err(anyhow!("The system is already downloading a new system configuration."))).map_err(|_| anyhow!("channel closed before we could send the response"))?;
                    }
                    AgentStateStatus::SwitchingToConfiguration { .. } => {
                        resp_tx.send(Err(anyhow!("The system is already switching to a new system configuration."))).map_err(|_| anyhow!("channel closed before we could send the response"))?;
                    }
                    AgentStateStatus::Standby => {
                        let system_package_id_arc = Arc::new(system_package_id.clone());
                        state.mark_switching_new_system(system_package_id, package_ids.clone())?;

                        let input_tx_clone = input_tx.clone();
                        let downloader_input = downloader.input();
                        let unpacker_input = unpacker.input();
                        let dbus_connection_input = dbus_connection.input();
                        // A bit annoying that we have to grab this from agent state, but seems like the better option. There are other ways to structure the code here to allow moving this stuff all inside the agent state so we don't need to clone the agent state or make an Arc or whatever, but I think this is fine for now.
                        let switch_start_file_path = state.absolute_switch_start_time_path();
                        let new_configuration_path = state.new_configuration_system_package_path().unwrap(); // We just marked that we're switching to a new system, so the `unwrap()` should never fail.
                        // We send the response just before starting the task just to try to avoid as much as possible any issues with never sending a response back if the system switch is almost immediate (e.g. everything already downloaded).
                        // TODO: guarantee that we'll wait until a response is sent back all the way through the server before we proceed with system switch?
                        resp_tx.send(Ok(())).map_err(|_| anyhow!("channel closed before we could send the response"))?;
                        pending_system_switch_task = Some(tokio::spawn(async move {
                            let download_timer = metrics::system::configuration_download_duration(&system_package_id_arc).start_timer();
                            let res = match downloader_input.download_packages(package_ids).await {
                                Ok(v) => v,
                                Err(err) => {
                                    tracing::error!(?err, "Got an error when downloading packages during system switch.");
                                    input_tx_clone.send(StateKeeperRequest::ConfigurationSwitchStartResult(Err(err))).await.unwrap();
                                    return;
                                },
                            };
                            let download_duration = download_timer.stop_and_record();
                            tracing::info!(download_duration_secs = download_duration.as_secs_f32(), "Finished downloading new system configuration.");

                            let setup_timer = metrics::system::configuration_setup_duration(&system_package_id_arc).start_timer();
                            match unpacker_input.unpack_downloads(res).await {
                                Ok(()) => (),
                                Err(err) => {
                                    tracing::error!(?err, "Got an error when unpacking downloads during system switch.");
                                    input_tx_clone.send(StateKeeperRequest::ConfigurationSwitchStartResult(Err(err))).await.unwrap();
                                    return;
                                }
                            };
                            let setup_duration = setup_timer.stop_and_record();
                            tracing::info!(setup_duration_secs = setup_duration.as_secs_f32(), "Finished unpacking new system configuration.");

                            record_switch_start(switch_start_file_path.clone()).unwrap();
                            match dbus_connection_input.perform_configuration_switch(new_configuration_path).await {
                                Ok(()) => (),
                                Err(err) => {
                                    tracing::error!(?err, "Got an error when performing a system switch after unpacking all downloads.");
                                    input_tx_clone.send(StateKeeperRequest::ConfigurationSwitchStartResult(Err(err))).await.unwrap();
                                    return;
                                }
                            }

                            // We'll check if system switch was made successfully inside the state keeper code instead of this ad-hoc task.
                            input_tx_clone.send(StateKeeperRequest::ConfigurationSwitchStartResult(Ok(()))).await.unwrap();
                        }));
                    }
                }
            }
            StateKeeperRequest::ConfigurationSwitchStartResult(Err(err)) => {
                pending_system_switch_task = None;

                let switch_duration =
                    calculate_switch_duration(state.absolute_switch_start_time_path()).unwrap();
                metrics::system::configuration_switch_duration(&Arc::new(
                    state.latest_package_id(),
                ))
                .observe(switch_duration.as_nanos().try_into().unwrap());
                tracing::info!(
                    switch_duration_secs = switch_duration.as_secs_f32(),
                    ?err,
                    "Failed to switch to new system configuration."
                );
            }
            StateKeeperRequest::ConfigurationSwitchStartResult(Ok(())) => {
                tracing::info!("Configuration switch was successful!");
                wait_for_system_update_and_update_state(&mut state, &dbus_connection).await?;
                pending_system_switch_task = None;
                tracing::info!("State updated!");

                let switch_duration =
                    calculate_switch_duration(state.absolute_switch_start_time_path()).unwrap();
                metrics::system::configuration_switch_duration(&Arc::new(
                    state.latest_package_id(),
                ))
                .observe(switch_duration.as_nanos().try_into().unwrap());
                tracing::info!(
                    switch_duration_secs = switch_duration.as_secs_f32(),
                    "Finished switching to new system configuration."
                );

                input_tx
                    .send(StateKeeperRequest::CleanupConfigurationHistory)
                    .await?;
            }
            StateKeeperRequest::CleanupConfigurationHistory => {
                tracing::info!("Cleaning up configuration history.");
                state.cleanup_configuration_history().await?;

                if state.has_packages_to_cleanup() {
                    let input_tx_clone = input_tx.clone();
                    let deleter_input = deleter.input();
                    let packages_to_cleanup = state.packages_to_cleanup();
                    pending_package_delete_task = Some(tokio::spawn(async move {
                        let res = deleter_input.delete_packages(packages_to_cleanup).await;
                        input_tx_clone
                            .send(StateKeeperRequest::PackageDeletionResult(res))
                            .await
                            .unwrap();
                    }));
                }
            }
            StateKeeperRequest::PackageDeletionResult(Ok(())) => {
                state.clear_packages_to_cleanup().await?;
                pending_package_delete_task = None;
            }
            StateKeeperRequest::PackageDeletionResult(Err(err)) => {
                tracing::error!(?err, "We failed to delete some packages to cleanup!");
                pending_package_delete_task = None;
            }
            StateKeeperRequest::GetSummary { resp_tx } => {
                resp_tx.send(Ok(state.summary())).unwrap();
            }
        }
    }

    tracing::info!("State keeper exited its main loop, will continue shutting down.");

    if let Some(task) = pending_clean_up_task {
        tracing::info!("We have a pending clean up task, waiting for it to finish.");
        task.await?;
    }

    if let Some(task) = pending_system_switch_task {
        tracing::info!("We have a pending system switch task, but we'll abort it because it could be the task getting us to shut down.");
        task.abort();
    }

    if let Some(task) = pending_package_delete_task {
        tracing::info!("We have a pending package deletion task, waiting for it to finish.");
        task.await?;
    }

    let shutdown_results = tokio::join!(
        downloader.shutdown(),
        unpacker.shutdown(),
        dbus_connection.shutdown(),
        deleter.shutdown(),
    );
    [
        shutdown_results.0,
        shutdown_results.1,
        shutdown_results.2,
        shutdown_results.3,
    ]
    .into_iter()
    .collect::<Result<_, _>>()?;

    tracing::info!("State keeper has finished shutting down.");
    Ok(())
}

async fn wait_for_system_update_and_update_state(
    state: &mut AgentState,
    dbus_connection: &StartedDBusConnection,
) -> anyhow::Result<()> {
    let state_base_dir = state.base_dir();

    loop {
        match check_switching_status(&state_base_dir).await? {
            SystemSwitchStatus::Successful { reboot_required } => {
                // TODO: check if we have to reboot also due to a kernel or some other thing that changed that requires a reboot. If we do, only consider things successful after the reboot. https://github.com/thefossguy/nixos-needsreboot
                state.mark_new_system_successful().await?;
                break;
            }
            SystemSwitchStatus::InProgress => {
                dbus_connection.wait_configuration_switch_complete().await?;
                // After the wait, we'll continue through the loop so we can evaluate the results once again.
            }
            SystemSwitchStatus::Failed(_) => {
                state.mark_new_system_failed().await?;
                break;
            }
        }
    }

    Ok(())
}
