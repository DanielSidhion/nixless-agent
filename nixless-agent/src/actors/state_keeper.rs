use std::path::PathBuf;

use anyhow::anyhow;
use derive_builder::Builder;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tracing::instrument;

use crate::{
    dbus_connection::DBusConnection,
    path_utils::clean_up_nix_dir,
    state::{
        check_switching_status, clean_up_system_switch_tracking_files, AgentState, AgentStateStatus,
    },
};

use super::{StartedDownloader, StartedUnpacker};

#[derive(Builder)]
#[builder(pattern = "owned")]
pub struct StateKeeper {
    directory: PathBuf,
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
            match state_keeper_task(self.directory, self.downloader, input_rx, input_tx_clone).await
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
    CleanUpDir,
    CleanUpDirResult(anyhow::Result<()>),
    UpdateToNewSystem {
        toplevel_path_string: String,
        paths: Vec<String>,
        resp_tx: oneshot::Sender<anyhow::Result<()>>,
    },
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

    pub async fn switch_to_new_system(
        &self,
        toplevel_path_string: String,
        paths: Vec<String>,
    ) -> anyhow::Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.input_tx
            .send(StateKeeperRequest::UpdateToNewSystem {
                toplevel_path_string,
                paths,
                resp_tx,
            })
            .await?;

        resp_rx.await?
    }
}

#[instrument(skip_all)]
async fn state_keeper_task(
    directory: PathBuf,
    downloader: StartedDownloader,
    input_rx: mpsc::Receiver<StateKeeperRequest>,
    input_tx: mpsc::Sender<StateKeeperRequest>,
) -> anyhow::Result<()> {
    tracing::info!("Checking if we can possibly be authorised to manage systemd units.");

    let dbus_connection = DBusConnection::new().start();
    if !dbus_connection.check_authorisation_possibility().await? {
        return Err(anyhow!(
            "we're not authorised to manage systemd units, so we won't be able to switch systems"
        ));
    }

    tracing::info!("We might be authorised to manage systemd units, continuing initialisation.");

    let mut state = AgentState::from_directory(directory.clone()).await?;

    // If we're here, we just got started, so we'll check what was our previous status and figure out next steps from there.
    match state.status() {
        AgentStateStatus::Temporary => unreachable!("Temporary agent status should be unreachable"),
        AgentStateStatus::New | AgentStateStatus::Standby => {
            // We can start operating normally, but we'll enqueue a job to clean up the directory.
            state.set_standby()?;
            input_tx.send(StateKeeperRequest::CleanUpDir).await?;
        }
        AgentStateStatus::FailedSwitch { .. } => {
            // We'll start in a "read-only" mode.
        }
        AgentStateStatus::DownloadingNewSystem { configuration } => {
            // We'll continue downloading the new system, but aside from that will operate normally.
            downloader
                .download_paths(configuration.paths.clone())
                .await?;
        }
        AgentStateStatus::SwitchingToNewSystem { .. } => {
            // We must check whether we switched successfully or not. In case the system switch task isn't yet complete, we'll loop again once it is complete so we can evaluate what to do.
            loop {
                let switching_status = check_switching_status(&directory).await?;
                match (
                    switching_status.started,
                    switching_status.finished,
                    switching_status.successful,
                ) {
                    (true, true, true) => {
                        // TODO: check if we have to reboot due to a kernel or some other thing that changed that requires a reboot. If we do, only consider things successful after the reboot.
                        clean_up_system_switch_tracking_files(&directory).await?;
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
                        dbus_connection.wait_system_switch_complete().await?;
                        // After the wait, we'll continue through the loop so we can evaluate the results once again.
                        // TODO: detect when we're stuck in an infinite loop and bail.
                    }
                }
            }
        }
    }

    tracing::info!("State keeper finished early status decision-making, will now enter its main processing loop.");

    let mut input_stream = ReceiverStream::new(input_rx);
    let mut pending_clean_up_task: Option<JoinHandle<()>> = None;

    while let Some(req) = input_stream.next().await {
        match req {
            StateKeeperRequest::CleanUpDir => {
                let input_tx_clone = input_tx.clone();
                let dir = directory.clone();
                pending_clean_up_task = Some(tokio::spawn(async move {
                    let res = clean_up_nix_dir(dir).await;
                    // TODO: deal with error when sending the result back.
                    input_tx_clone
                        .send(StateKeeperRequest::CleanUpDirResult(res))
                        .await
                        .unwrap();
                }));
            }
            StateKeeperRequest::CleanUpDirResult(Err(err)) => {
                tracing::error!(?err, "We failed to clean up the nix state directory!");
                pending_clean_up_task = None;
            }
            StateKeeperRequest::CleanUpDirResult(Ok(())) => {
                pending_clean_up_task = None;
            }
            StateKeeperRequest::UpdateToNewSystem {
                toplevel_path_string,
                paths,
                resp_tx,
            } => {
                match state.status() {
                    AgentStateStatus::New | AgentStateStatus::Temporary => unreachable!("should have never been in a new or temporary state during the state keeper main loop"),
                    AgentStateStatus::FailedSwitch { .. } => {
                        resp_tx.send(Err(anyhow!("The system already failed a system switch and must be recovered before switching to a new configuration."))).map_err(|_| anyhow!("channel closed before we could send the response"))?;
                    }
                    AgentStateStatus::DownloadingNewSystem { .. } => {
                        resp_tx.send(Err(anyhow!("The system is already downloading a new system configuration."))).map_err(|_| anyhow!("channel closed before we could send the response"))?;
                    }
                    AgentStateStatus::SwitchingToNewSystem { .. } => {
                        resp_tx.send(Err(anyhow!("The system is already switching to a new system configuration."))).map_err(|_| anyhow!("channel closed before we could send the response"))?;
                    }
                    AgentStateStatus::Standby => {
                        // TODO: go through with the request to update.
                        resp_tx.send(Ok(())).map_err(|_| anyhow!("channel closed before we could send the response"))?;
                    }
                }
            }
        }
    }

    tracing::info!("State keeper exited its main loop, will continue shutting down.");

    if let Some(task) = pending_clean_up_task {
        tracing::info!("We have a pending clean up task, waiting for it to finish.");
        task.await?;
    }

    Ok(())
}
