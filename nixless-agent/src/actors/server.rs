use actix_web::{
    error::InternalError, http::StatusCode, web, App, HttpRequest, HttpResponse, HttpServer,
    Responder,
};
use anyhow::anyhow;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tracing::instrument;

use super::StartedStateKeeper;

pub struct Server {
    port: u16,
    state_keeper: StartedStateKeeper,
}

impl Server {
    pub fn new(port: u16, state_keeper: StartedStateKeeper) -> Self {
        Self { port, state_keeper }
    }

    pub fn start(self) -> anyhow::Result<StartedServer> {
        let (input_tx, input_rx) = mpsc::channel(10);

        let inputs_task = tokio::spawn(server_task(input_rx, self.state_keeper));

        let inputs_sender = web::Data::new(input_tx.clone());
        let server_task = HttpServer::new(move || {
            App::new()
                .app_data(inputs_sender.clone())
                .route(
                    "/new-configuration",
                    web::post().to(parse_new_configuration),
                )
                .route("/", web::to(HttpResponse::ImATeapot))
        })
        .shutdown_timeout(5)
        .workers(2)
        .bind(("0.0.0.0", self.port))?
        .run();

        let server_task = tokio::spawn(async { server_task.await });

        Ok(StartedServer {
            inputs_task: Some(inputs_task),
            server_task: Some(server_task),
            input_tx,
        })
    }
}

pub struct StartedServer {
    inputs_task: Option<JoinHandle<anyhow::Result<()>>>,
    server_task: Option<JoinHandle<std::io::Result<()>>>,
    input_tx: mpsc::Sender<ServerRequest>,
}

pub enum ServerRequest {
    UpdateToNewSystem {
        toplevel_path_string: String,
        paths: Vec<String>,
        resp_tx: oneshot::Sender<anyhow::Result<()>>,
    },
}

#[instrument(skip_all)]
async fn server_task(
    input_rx: mpsc::Receiver<ServerRequest>,
    state_keeper: StartedStateKeeper,
) -> anyhow::Result<()> {
    let mut input_stream = ReceiverStream::new(input_rx);

    tracing::info!("Server will now enter its main loop.");

    while let Some(req) = input_stream.next().await {
        match req {
            ServerRequest::UpdateToNewSystem {
                toplevel_path_string,
                paths,
                resp_tx,
            } => {
                let res = state_keeper
                    .switch_to_new_system(toplevel_path_string, paths)
                    .await;
                resp_tx
                    .send(res)
                    .map_err(|_| anyhow!("channel closed before we could send the response"))?;
            }
        }
    }

    Ok(())
}

#[instrument(skip_all, fields(uri = req.uri().to_string(), method = req.method().as_str()))]
async fn parse_new_configuration(
    req: HttpRequest,
    payload_string: String,
    inputs_sender: web::Data<mpsc::Sender<ServerRequest>>,
) -> actix_web::Result<impl Responder> {
    let mut lines = payload_string.lines();

    if let Some(toplevel_path_string) = lines.next() {
        let paths: Vec<_> = lines.map(str::to_string).collect();

        let (resp_tx, resp_rx) = oneshot::channel();

        inputs_sender
            .send(ServerRequest::UpdateToNewSystem {
                toplevel_path_string: toplevel_path_string.to_string(),
                paths,
                resp_tx,
            })
            .await
            .map_err(|err| InternalError::new(err, StatusCode::INTERNAL_SERVER_ERROR))?;

        match resp_rx
            .await
            .map_err(|err| InternalError::new(err, StatusCode::INTERNAL_SERVER_ERROR))?
        {
            Ok(()) => Ok(HttpResponse::NoContent().finish()),
            Err(err) => Ok(HttpResponse::Conflict().body(err.to_string())),
        }
    } else {
        Ok(HttpResponse::BadRequest().finish())
    }
}
