use std::{collections::HashMap, ops::Deref, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{anyhow, Context};
use dbus::{
    arg::{RefArg, Variant},
    nonblock::{stdintf::org_freedesktop_dbus::Properties, Proxy, SyncConnection},
    Path,
};
use derive_builder::Builder;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};

const TRANSIENT_SERVICE_NAME: &str = "nixless-agent-system-switch.service";

#[derive(Builder)]
pub struct DBusConnection {
    relative_configuration_activation_command: PathBuf,
    absolute_activation_tracker_command: PathBuf,
    activation_track_dir: PathBuf,
}

impl DBusConnection {
    pub fn builder() -> DBusConnectionBuilder {
        DBusConnectionBuilder::default()
    }

    pub fn start(self) -> StartedDBusConnection {
        let (input_tx, input_rx) = mpsc::channel(10);

        let input_tx_clone = input_tx.clone();
        let task = tokio::spawn(async {
            match dbus_connection_task(
                input_rx,
                input_tx_clone,
                self.relative_configuration_activation_command,
                self.absolute_activation_tracker_command,
                self.activation_track_dir,
            )
            .await
            {
                Ok(()) => Ok(()),
                Err(err) => {
                    tracing::error!(
                        ?err,
                        "The D-Bus connection task encountered a fatal error and has stopped."
                    );
                    Err(err)
                }
            }
        });

        StartedDBusConnection {
            task,
            input: StartedDBusConnectionInput { input_tx },
        }
    }
}

#[derive(Debug)]
pub struct StartedDBusConnection {
    task: JoinHandle<anyhow::Result<()>>,
    input: StartedDBusConnectionInput,
}

impl StartedDBusConnection {
    pub fn input(&self) -> StartedDBusConnectionInput {
        self.input.clone()
    }

    pub async fn shutdown(self) -> anyhow::Result<()> {
        self.input
            .input_tx
            .send(DBusConnectionRequest::Shutdown)
            .await?;
        self.task.await?
    }
}

impl Deref for StartedDBusConnection {
    type Target = StartedDBusConnectionInput;

    fn deref(&self) -> &Self::Target {
        &self.input
    }
}

#[derive(Clone, Debug)]
pub struct StartedDBusConnectionInput {
    input_tx: mpsc::Sender<DBusConnectionRequest>,
}

impl StartedDBusConnectionInput {
    pub async fn check_authorisation_possibility(&self) -> anyhow::Result<bool> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.input_tx
            .send(DBusConnectionRequest::CheckAuthorisationPossibility { resp_tx })
            .await?;
        resp_rx.await?
    }

    pub async fn perform_configuration_switch(
        &self,
        system_package_path: PathBuf,
    ) -> anyhow::Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.input_tx
            .send(DBusConnectionRequest::PerformConfigurationSwitch {
                system_package_path,
                resp_tx,
            })
            .await?;
        resp_rx.await?
    }

    pub async fn wait_configuration_switch_complete(&self) -> anyhow::Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.input_tx
            .send(DBusConnectionRequest::WaitConfigurationSwitchComplete { resp_tx })
            .await?;
        resp_rx.await?
    }
}

pub enum DBusConnectionRequest {
    CheckAuthorisationPossibility {
        resp_tx: oneshot::Sender<anyhow::Result<bool>>,
    },
    PerformConfigurationSwitch {
        system_package_path: PathBuf,
        resp_tx: oneshot::Sender<anyhow::Result<()>>,
    },
    WaitConfigurationSwitchComplete {
        resp_tx: oneshot::Sender<anyhow::Result<()>>,
    },
    ClearPendingSwitchTask,
    Shutdown,
}

async fn dbus_connection_task(
    input_rx: mpsc::Receiver<DBusConnectionRequest>,
    input_tx: mpsc::Sender<DBusConnectionRequest>,
    relative_configuration_activation_command: PathBuf,
    absolute_activation_tracker_command: PathBuf,
    activation_track_dir: PathBuf,
) -> anyhow::Result<()> {
    let (resource, conn) = dbus_tokio::connection::new_system_sync()?;

    let dbus_task = tokio::spawn(async move {
        let err = resource.await;
        // TODO: send signal to the rest of the application, or do something better here.
        panic!("D-Bus got disconnected with the following error: {}", err);
    });

    let mut input_stream = ReceiverStream::new(input_rx);

    tracing::info!(
        "D-Bus connection has finished initialisation and will now enter its main loop."
    );

    let mut pending_switch_task: Option<JoinHandle<anyhow::Result<()>>> = None;

    while let Some(req) = input_stream.next().await {
        match req {
            DBusConnectionRequest::Shutdown => {
                tracing::info!("D-Bus connection got a request to shut down. Proceeding.");
                break;
            }
            DBusConnectionRequest::ClearPendingSwitchTask => {
                if pending_switch_task.is_none() {
                    tracing::error!("D-Bus connection got a request to clear pending configuration switch task, but it's already cleared!");
                    continue;
                }

                pending_switch_task = None;
            }
            DBusConnectionRequest::CheckAuthorisationPossibility { resp_tx } => {
                let res = check_polkit_authorised(conn.clone()).await;
                resp_tx
                    .send(res)
                    .map_err(|_| anyhow!("channel closed before we could send the response"))?;
            }
            DBusConnectionRequest::PerformConfigurationSwitch {
                system_package_path,
                resp_tx,
            } => {
                if pending_switch_task.is_some() {
                    tracing::error!("D-Bus connection got a request to perform a configuration switch while performing a configuration switch already! This should've never happened.");
                    panic!("Got a request to perform configuration switch in the middle of a configuration switch");
                }

                let activation_command_path =
                    system_package_path.join(&relative_configuration_activation_command);

                let conn_clone = conn.clone();
                let absolute_activation_tracker_command_clone =
                    absolute_activation_tracker_command.clone();
                let activation_track_dir_clone = activation_track_dir.clone();
                let input_tx_clone = input_tx.clone();
                pending_switch_task = Some(tokio::spawn(async move {
                    let res = perform_configuration_switch(
                        conn_clone,
                        activation_command_path,
                        &absolute_activation_tracker_command_clone,
                        &activation_track_dir_clone,
                    )
                    .await;
                    resp_tx
                        .send(res)
                        .map_err(|_| anyhow!("channel closed before we could send the response"))?;
                    input_tx_clone
                        .send(DBusConnectionRequest::ClearPendingSwitchTask)
                        .await
                        .unwrap();
                    Ok(())
                }));
            }
            DBusConnectionRequest::WaitConfigurationSwitchComplete { resp_tx } => {
                let res = wait_configuration_switch_complete(conn.clone()).await;
                resp_tx
                    .send(res)
                    .map_err(|_| anyhow!("channel closed before we could send the response"))?;
            }
        }
    }

    tracing::info!("D-Bus connection task exited its main loop, will proceed shutting down.");

    if let Some(task) = pending_switch_task {
        tracing::info!("D-Bus connection task had a pending configuration switch task. Will abort it because it could be the task that caused the shut down to happen.");
        task.abort();
    }

    tracing::info!("Will now abort the connection to the system bus.");
    dbus_task.abort();
    tracing::info!("D-Bus connection has finished shutting down.");
    Ok(())
}

async fn check_polkit_authorised(conn: Arc<SyncConnection>) -> anyhow::Result<bool> {
    let conn_name = conn.unique_name().to_string();

    // https://www.freedesktop.org/software/polkit/docs/latest/eggdbus-interface-org.freedesktop.PolicyKit1.Authority.html
    let polkit_proxy = Proxy::new(
        "org.freedesktop.PolicyKit1",
        "/org/freedesktop/PolicyKit1/Authority",
        Duration::from_millis(1000),
        conn,
    );

    let mut subject_details: HashMap<&str, dbus::arg::Variant<&str>> = HashMap::new();
    subject_details.insert("name", dbus::arg::Variant(&conn_name));
    let action_details: HashMap<&str, &str> = HashMap::new();

    let ((is_authorised, is_challenge, _details),): ((bool, bool, HashMap<String, String>),) =
        polkit_proxy
            .method_call(
                "org.freedesktop.PolicyKit1.Authority",
                "CheckAuthorization",
                // https://www.freedesktop.org/software/polkit/docs/latest/eggdbus-interface-org.freedesktop.PolicyKit1.Authority.html#eggdbus-method-org.freedesktop.PolicyKit1.Authority.CheckAuthorization
                (
                    ("system-bus-name", subject_details),
                    "org.freedesktop.systemd1.manage-units",
                    action_details,
                    0u32,
                    "",
                ),
            )
            .await?;

    // We'll never fully know if we are 100% authorised until we actually try to perform the action because we can't pass details on the check to policy kit, so this is the best we can do.
    Ok(is_authorised || is_challenge)
}

#[tracing::instrument(skip_all)]
async fn perform_configuration_switch(
    conn: Arc<SyncConnection>,
    activation_command_path: PathBuf,
    absolute_activation_tracker_command: &PathBuf,
    activation_track_dir: &PathBuf,
) -> anyhow::Result<()> {
    // https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.systemd1.html
    let systemd_proxy = Proxy::new(
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        Duration::from_millis(1000),
        conn.clone(),
    );

    tracing::info!(activation_command_path = ?activation_command_path.to_str(), "Will start a system switch.");

    let aux_not_used: Vec<(String, Vec<(String, Variant<&str>)>)> = Vec::new();
    let transient_service_properties = build_transient_service_properties(
        activation_command_path,
        absolute_activation_tracker_command,
        activation_track_dir,
    )?;

    let (job_path,): (Path,) = systemd_proxy
        .method_call(
            "org.freedesktop.systemd1.Manager",
            "StartTransientUnit",
            (
                TRANSIENT_SERVICE_NAME,
                "fail",
                transient_service_properties,
                aux_not_used,
            ),
        )
        .await?;

    let job_proxy = Proxy::new(
        "org.freedesktop.systemd1",
        job_path,
        Duration::from_millis(1000),
        conn.clone(),
    );

    // We'll keep checking until the job is running or done (means it doesn't exist anymore).
    loop {
        match job_proxy
            .get::<String>("org.freedesktop.systemd1.Job", "State")
            .await
        {
            Ok(state) => {
                if state == "running" {
                    // Means we can get a unit object already, so we'll stop checking for the job specifically. In theory we could only rely on whether the job exists or not, but we want to check the unit to make sure it will not be kept around once it's done.
                    break;
                }

                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            Err(err) => {
                if let Some("org.freedesktop.DBus.Error.UnknownObject") = err.name() {
                    // Job is finished running.
                    break;
                }

                return Err(err).context("trying to get status of the job we created");
            }
        }
    }

    wait_configuration_switch_complete(conn.clone()).await?;
    Ok(())
}

#[tracing::instrument(skip_all)]
async fn wait_configuration_switch_complete(conn: Arc<SyncConnection>) -> anyhow::Result<()> {
    let systemd_proxy = Proxy::new(
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        Duration::from_millis(1000),
        conn.clone(),
    );

    let (unit_path,): (Path,) = match systemd_proxy
        .method_call(
            "org.freedesktop.systemd1.Manager",
            "GetUnit",
            (TRANSIENT_SERVICE_NAME,),
        )
        .await
    {
        Ok(v) => v,
        Err(err) => {
            if let Some("org.freedesktop.systemd1.NoSuchUnit") = err.name() {
                // Means the service has already stopped, so there's nothing else for us to do here.
                return Ok(());
            }

            return Err(err).context("trying to get the path to the unit we started");
        }
    };

    let unit_proxy = Proxy::new(
        "org.freedesktop.systemd1",
        unit_path,
        Duration::from_millis(1000),
        conn,
    );

    loop {
        match unit_proxy
            .get::<String>("org.freedesktop.systemd1.Unit", "ActiveState")
            .await
        {
            Ok(state) => {
                if state == "inactive" {
                    break;
                }

                if state == "activating" || state == "deactivating" {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                if state == "active" || state == "reloading" || state == "failed" {
                    return Err(anyhow!("when waiting for the systemd switch unit to finish, it entered a state we were not expecting"));
                }
            }
            Err(err) => {
                tracing::error!(
                    "We got the following error when checking for the unit: {:?} message {:?}",
                    err.name(),
                    err.message()
                );
                break;
            }
        }
    }

    Ok(())
}

fn build_transient_service_properties(
    activation_command_path: PathBuf,
    absolute_activation_tracker_command: &PathBuf,
    activation_track_dir: &PathBuf,
) -> anyhow::Result<Vec<(&'static str, Variant<Box<dyn RefArg>>)>> {
    let activation_command_path_string = activation_command_path
        .to_str()
        .ok_or_else(|| anyhow!("The path to the activation command can't be converted to utf-8"))?
        .to_string();
    let activation_tracker_command_path_string = absolute_activation_tracker_command
        .to_str()
        .ok_or_else(|| {
            anyhow!("The path to the activation tracking command can't be converted to utf-8")
        })?
        .to_string();
    let activation_track_dir_string = activation_track_dir
        .to_str()
        .ok_or_else(|| {
            anyhow!("The path to the activation tracking directory can't be converted to utf-8")
        })?
        .to_string();

    let mut res: Vec<(&str, Variant<Box<dyn RefArg>>)> = Vec::new();

    res.push(("Description", Variant(Box::new("A transient service responsible for switching the system to its new configuration. Started by nixless-agent.".to_string()))));
    // https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.systemd1.html#Properties2
    // Discovered empirically that it doesn't need the runtime-related information. This is all that is needed:
    // - the binary path to execute.
    // - an array with all arguments to pass to the executed command, starting with argument 0.
    // - a boolean whether it should be considered a failure if the process exits uncleanly.
    // a(sasb)
    let exec_start: Vec<(String, Vec<String>, bool)> = vec![(
        activation_command_path_string.clone(),
        vec![activation_command_path_string, "switch".to_string()],
        false,
    )];
    let exec_start_pre: Vec<(String, Vec<String>, bool)> = vec![(
        activation_tracker_command_path_string.clone(),
        vec![
            activation_tracker_command_path_string.clone(),
            "pre-switch".to_string(),
            activation_track_dir_string.clone(),
            "nixless-agent".to_string(),
        ],
        false,
    )];
    let exec_start_post: Vec<(String, Vec<String>, bool)> = vec![(
        activation_tracker_command_path_string.clone(),
        vec![
            activation_tracker_command_path_string.clone(),
            "switch-success".to_string(),
            activation_track_dir_string.clone(),
            "nixless-agent".to_string(),
        ],
        false,
    )];
    let exec_stop_post: Vec<(String, Vec<String>, bool)> = vec![(
        activation_tracker_command_path_string.clone(),
        vec![
            activation_tracker_command_path_string.clone(),
            "post-switch".to_string(),
            activation_track_dir_string.clone(),
            "nixless-agent".to_string(),
        ],
        false,
    )];
    res.push(("ExecStart", Variant(Box::new(exec_start))));
    res.push(("ExecStartPre", Variant(Box::new(exec_start_pre))));
    res.push(("ExecStartPost", Variant(Box::new(exec_start_post))));
    res.push(("ExecStopPost", Variant(Box::new(exec_stop_post))));
    res.push(("Type", Variant(Box::new("oneshot".to_string()))));
    res.push(("RefuseManualStop", Variant(Box::new(true))));
    res.push(("RemainAfterExit", Variant(Box::new(false))));
    // We already have the ExecStartPost/ExecStopPost commands to tell us whether the switch succeeded or failed, so we don't need systemd to keep the unit around if it fails.
    res.push((
        "CollectMode",
        Variant(Box::new("inactive-or-failed".to_string())),
    ));

    Ok(res)
}
