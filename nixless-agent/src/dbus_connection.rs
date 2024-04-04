use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{anyhow, Context};
use dbus::{
    arg::{RefArg, Variant},
    nonblock::{stdintf::org_freedesktop_dbus::Properties, Proxy, SyncConnection},
    Path,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

pub struct DBusConnection;

impl DBusConnection {
    pub fn new() -> Self {
        Self
    }

    pub fn start(self) -> StartedDBusConnection {
        let (input_tx, input_rx) = mpsc::channel(10);

        let task = tokio::spawn(dbus_connection_task(input_rx));

        StartedDBusConnection {
            input_tx,
            task: Some(task),
        }
    }
}

pub struct StartedDBusConnection {
    task: Option<JoinHandle<anyhow::Result<()>>>,
    input_tx: mpsc::Sender<DBusConnectionRequest>,
}

impl StartedDBusConnection {
    pub fn child(&self) -> Self {
        Self {
            task: None,
            input_tx: self.input_tx.clone(),
        }
    }

    pub async fn shutdown(self) -> anyhow::Result<()> {
        if let Some(task) = self.task {
            task.await??;
        }

        Ok(())
    }

    pub async fn check_authorisation_possibility(&self) -> anyhow::Result<bool> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.input_tx
            .send(DBusConnectionRequest::CheckAuthorisationPossibility { resp_tx })
            .await?;
        resp_rx.await?
    }

    pub async fn perform_system_switch(&self) -> anyhow::Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.input_tx
            .send(DBusConnectionRequest::PerformSystemSwitch { resp_tx })
            .await?;
        resp_rx.await?
    }
}

pub enum DBusConnectionRequest {
    CheckAuthorisationPossibility {
        resp_tx: oneshot::Sender<anyhow::Result<bool>>,
    },
    PerformSystemSwitch {
        resp_tx: oneshot::Sender<anyhow::Result<()>>,
    },
}

async fn dbus_connection_task(
    mut input_rx: mpsc::Receiver<DBusConnectionRequest>,
) -> anyhow::Result<()> {
    let (resource, conn) = dbus_tokio::connection::new_system_sync()?;

    let dbus_task = tokio::spawn(async move {
        let err = resource.await;
        // TODO: send signal to the rest of the application, or do something better here.
        panic!("D-Bus got disconnected with the following error: {}", err);
    });

    loop {
        tokio::select! {
            req = input_rx.recv() => {
                match req {
                    None => break,
                    Some(DBusConnectionRequest::CheckAuthorisationPossibility { resp_tx }) => {
                        let res = check_polkit_authorised(conn.clone()).await;
                        resp_tx.send(res).map_err(|_| anyhow!("channel closed before we could send the response"))?;
                    }
                    Some(DBusConnectionRequest::PerformSystemSwitch { resp_tx }) => {
                        let res = perform_system_switch(conn.clone()).await;
                        resp_tx.send(res).map_err(|_| anyhow!("channel closed before we could send the response"))?;
                    }
                }
            }
        }
    }

    dbus_task.abort();

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

    // We'll never fully know if we are 100% authorised until we actually try to perform the action because we can't check if we're authorised for the particular service we want to start, so this is the best we can do.
    Ok(is_authorised || is_challenge)
}

async fn perform_system_switch(conn: Arc<SyncConnection>) -> anyhow::Result<()> {
    // https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.systemd1.html
    let systemd_proxy = Proxy::new(
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        Duration::from_millis(1000),
        conn.clone(),
    );

    let aux_not_used: Vec<(String, Vec<(String, Variant<&str>)>)> = Vec::new();
    let mut transient_service_properties: Vec<(&str, Variant<Box<dyn RefArg>>)> = Vec::new();
    transient_service_properties.push(("Description", Variant(Box::new("A transient service responsible for switching the system to its new configuration. Started by nixless-agent.".to_string()))));
    // https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.systemd1.html#Properties2
    // Discovered empirically that it doesn't need the runtime-related information. This is all that is needed:
    // - the binary path to execute.
    // - an array with all arguments to pass to the executed command, starting with argument 0.
    // - a boolean whether it should be considered a failure if the process exits uncleanly.
    // a(sasb)
    let exec_start: Vec<(String, Vec<String>, bool)> = vec![(
        // "/usr/bin/bash".to_string(),
        // vec![
        //     "/usr/bin/bash".to_string(),
        //     "-c".to_string(),
        //     "sleep 3".to_string(),
        // ],
        "/usr/bin/false".to_string(),
        vec!["/usr/bin/false".to_string()],
        false,
    )];
    let exec_start_pre: Vec<(String, Vec<String>, bool)> = vec![(
        "/usr/bin/touch".to_string(),
        vec!["/usr/bin/touch".to_string(), "/tmp/beforeexec".to_string()],
        false,
    )];
    let exec_start_post: Vec<(String, Vec<String>, bool)> = vec![(
        "/usr/bin/touch".to_string(),
        vec!["/usr/bin/touch".to_string(), "/tmp/afterexec".to_string()],
        false,
    )];
    // TODO: use $SERVICE_RESULT, $EXIT_CODE and $EXIT_STATUS on ExecStopPost?
    let exec_stop_post: Vec<(String, Vec<String>, bool)> = vec![(
        "/usr/bin/touch".to_string(),
        vec!["/usr/bin/touch".to_string(), "/tmp/afterstop".to_string()],
        false,
    )];
    transient_service_properties.push(("ExecStart", Variant(Box::new(exec_start))));
    transient_service_properties.push(("ExecStartPre", Variant(Box::new(exec_start_pre))));
    transient_service_properties.push(("ExecStartPost", Variant(Box::new(exec_start_post))));
    transient_service_properties.push(("ExecStopPost", Variant(Box::new(exec_stop_post))));
    transient_service_properties.push(("Type", Variant(Box::new("oneshot".to_string()))));
    transient_service_properties.push(("RefuseManualStop", Variant(Box::new(true))));
    transient_service_properties.push(("RemainAfterExit", Variant(Box::new(false))));
    // We already have the ExecStartPost/ExecStopPost commands to tell us whether the switch succeeded or failed, so we don't need systemd to keep the unit around if it fails.
    transient_service_properties.push((
        "CollectMode",
        Variant(Box::new("inactive-or-failed".to_string())),
    ));

    let (job_path,): (Path,) = systemd_proxy
        .method_call(
            "org.freedesktop.systemd1.Manager",
            "StartTransientUnit",
            (
                "nixless-agent-system-switch.service",
                "fail",
                transient_service_properties,
                aux_not_used,
            ),
        )
        .await?;

    // Now we'll keep waiting for the switch to finish.
    // First we make sure the job is done.
    let job_proxy = Proxy::new(
        "org.freedesktop.systemd1",
        job_path,
        Duration::from_millis(1000),
        conn.clone(),
    );

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

                println!("Job is in state {}, sleeping for 500ms", state);
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            Err(err) => {
                if let Some("org.freedesktop.DBus.Error.UnknownObject") = err.name() {
                    // Job is finished running, so we'll check for the status of the unit next.
                    break;
                }

                return Err(err).context("trying to get status of the job we created");
            }
        }
    }

    let (unit_path,): (Path,) = match systemd_proxy
        .method_call(
            "org.freedesktop.systemd1.Manager",
            "GetUnit",
            ("nixless-agent-system-switch.service",),
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

    println!("Unit path is {}", unit_path.to_string());

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
                    println!("Unit is inactive, nothing else to do!");
                    break;
                }

                if state == "activating" || state == "deactivating" {
                    println!("Unit is in state {}, sleeping for 500ms", state);
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
                if state == "active" || state == "reloading" || state == "failed" {
                    println!("Unit is in state {}", state);
                    return Err(anyhow!("when waiting for the systemd switch unit to finish, it entered a state we were not expecting"));
                }
            }
            Err(err) => {
                println!(
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
