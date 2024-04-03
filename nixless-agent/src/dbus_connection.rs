use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::anyhow;
use dbus::{
    arg::{RefArg, Variant},
    nonblock::{Proxy, SyncConnection},
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

    pub async fn start_system_switch(&self) -> anyhow::Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.input_tx
            .send(DBusConnectionRequest::StartSystemSwitch { resp_tx })
            .await?;
        resp_rx.await?
    }
}

pub enum DBusConnectionRequest {
    CheckAuthorisationPossibility {
        resp_tx: oneshot::Sender<anyhow::Result<bool>>,
    },
    StartSystemSwitch {
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
                    Some(DBusConnectionRequest::StartSystemSwitch { resp_tx }) => {
                        let res = start_system_switch(conn.clone()).await;
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

async fn start_system_switch(conn: Arc<SyncConnection>) -> anyhow::Result<()> {
    // https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.systemd1.html
    let systemd_proxy = Proxy::new(
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        Duration::from_millis(1000),
        conn,
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

    // in  s name,
    // in  s mode,
    // in  a(sv) properties,
    // in  a(sa(sv)) aux,
    // out o job

    let (service_path,): (Path,) = systemd_proxy
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

    println!("Got service path {}", service_path);

    Ok(())
}
