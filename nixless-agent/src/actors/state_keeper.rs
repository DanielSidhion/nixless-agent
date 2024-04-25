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
    path_utils::clean_up_nix_var_dir,
    state::{
        check_switching_status, clean_up_system_switch_tracking_files, AgentState, AgentStateStatus,
    },
};

use super::{StartedDownloader, StartedUnpacker};

#[derive(Builder)]
#[builder(pattern = "owned")]
pub struct StateKeeper {
    state: AgentState,
    dbus_connection: StartedDBusConnection,
    downloader: StartedDownloader,
    unpacker: StartedUnpacker,
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
            task: Some(task),
            input_tx,
        }
    }
}

enum StateKeeperRequest {
    CleanUpStateDir,
    CleanUpStateDirResult(anyhow::Result<()>),
    SwitchToNewConfiguration {
        system_package_id: String,
        package_ids: Vec<String>,
        resp_tx: oneshot::Sender<anyhow::Result<()>>,
    },
    ConfigurationSwitchResult(anyhow::Result<()>),
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

    pub async fn switch_to_new_configuration(
        &self,
        system_package_id: String,
        package_ids: Vec<String>,
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
}

#[instrument(skip_all)]
async fn state_keeper_task(
    mut state: AgentState,
    dbus_connection: StartedDBusConnection,
    downloader: StartedDownloader,
    unpacker: StartedUnpacker,
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
    let state_base_dir = state.base_dir();

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
            // We must check whether we switched successfully or not. In case the system switch task isn't yet complete, we'll loop again once it is complete so we can evaluate what to do.
            loop {
                let switching_status = check_switching_status(&state_base_dir).await?;
                match (
                    switching_status.started,
                    switching_status.finished,
                    switching_status.successful,
                ) {
                    (true, true, true) => {
                        // TODO: check if we have to reboot due to a kernel or some other thing that changed that requires a reboot. If we do, only consider things successful after the reboot.
                        clean_up_system_switch_tracking_files(&state_base_dir).await?;
                        state.mark_new_system_successful().await?;
                        break;
                    }
                    (true, true, false) => {
                        let status_codes = switching_status.status_codes.unwrap();
                        if status_codes.service_result == "exit-code"
                            && status_codes.exit_status == "100"
                        {
                            // Switch was "successful", but requires a reboot. Only consider things successful after the reboot.
                            // TODO: reboot
                            break;
                        } else {
                            // We failed for real. We're in an inconsistent state, so we'll start in a "read-only" mode.
                            state.mark_new_system_failed().await?;
                            break;
                        }
                    }
                    (_, false, _) | (false, _, _) => {
                        dbus_connection.wait_configuration_switch_complete().await?;
                        // After the wait, we'll continue through the loop so we can evaluate the results once again.
                        // TODO: detect when we're stuck in an infinite loop and bail.
                    }
                }
            }
        }
    }

    tracing::info!("State keeper finished early status decision-making, will now enter its main processing loop.");

    let mut pending_clean_up_task: Option<JoinHandle<()>> = None;
    let mut pending_system_switch_task: Option<JoinHandle<()>> = None;

    while let Some(req) = input_stream.next().await {
        match req {
            StateKeeperRequest::CleanUpStateDir => {
                let input_tx_clone = input_tx.clone();
                let dir = state.base_dir_nix();
                tracing::info!("Starting a task to clean up the Nix state dir.");
                pending_clean_up_task = Some(tokio::spawn(async move {
                    let res = clean_up_nix_var_dir(dir).await;
                    // TODO: deal with error when sending the result back.
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
            StateKeeperRequest::SwitchToNewConfiguration {
                system_package_id,
                package_ids,
                resp_tx,
            } => {
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
                        state.mark_switching_new_system(system_package_id, package_ids.clone())?;

                        let input_tx_clone = input_tx.clone();
                        let downloader_child = downloader.child();
                        let unpacker_child = unpacker.child();
                        let dbus_connection_child = dbus_connection.child();
                        let new_configuration_path = state.new_configuration_system_package_path().unwrap(); // We just marked that we're switching to a new system, so the `unwrap()` should never fail.
                        // We send the response just before starting the task just to try to avoid as much as possible any issues with never sending a response back if the system switch is almost immediate (e.g. everything already downloaded).
                        // TODO: guarantee that we'll wait until a response is sent back all the way through the server before we proceed with system switch?
                        resp_tx.send(Ok(())).map_err(|_| anyhow!("channel closed before we could send the response"))?;
                        pending_system_switch_task = Some(tokio::spawn(async move {
                            let res = match downloader_child.download_packages(package_ids).await {
                                Ok(v) => v,
                                Err(err) => {
                                    tracing::error!(?err, "Got an error when downloading packages during system switch.");
                                    input_tx_clone.send(StateKeeperRequest::ConfigurationSwitchResult(Err(err))).await.unwrap();
                                    return;
                                },
                            };

                            match unpacker_child.unpack_downloads(res).await {
                                Ok(()) => (),
                                Err(err) => {
                                    tracing::error!(?err, "Got an error when unpacking downloads during system switch.");
                                    input_tx_clone.send(StateKeeperRequest::ConfigurationSwitchResult(Err(err))).await.unwrap();
                                    return;
                                }
                            };

                            match dbus_connection_child.perform_configuration_switch(new_configuration_path).await {
                                Ok(()) => (),
                                Err(err) => {
                                    tracing::error!(?err, "Got an error when performing a system switch after unpacking all downloads.");
                                    input_tx_clone.send(StateKeeperRequest::ConfigurationSwitchResult(Err(err))).await.unwrap();
                                    return;
                                }
                            }

                            // TODO: check if system switch was made successfully, similar code from the start up of the state keeper.
                            input_tx_clone.send(StateKeeperRequest::ConfigurationSwitchResult(Ok(()))).await.unwrap();
                        }));
                    }
                }
            }
            StateKeeperRequest::ConfigurationSwitchResult(Err(err)) => {
                tracing::error!(?err, "We failed to perform a system switch!");
                pending_system_switch_task = None;
            }
            StateKeeperRequest::ConfigurationSwitchResult(Ok(())) => {
                tracing::info!("System switch completed successfully!");
                state.mark_new_system_successful().await?;
                pending_system_switch_task = None;
            }
        }
    }

    tracing::info!("State keeper exited its main loop, will continue shutting down.");

    if let Some(task) = pending_clean_up_task {
        tracing::info!("We have a pending clean up task, waiting for it to finish.");
        task.await?;
    }

    if let Some(task) = pending_system_switch_task {
        tracing::info!("We have a pending system switch task, waiting for it to finish.");
        task.await?;
    }

    Ok(())
}
