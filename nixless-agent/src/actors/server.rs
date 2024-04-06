use actix_web::{
    error::InternalError, guard::fn_guard, http::StatusCode, web, App, HttpRequest, HttpResponse,
    HttpServer, Responder,
};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};

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
                    "/",
                    web::post()
                        .guard(fn_guard(|c| {
                            c.head().headers().get("content-length").is_some_and(|val| {
                                val.to_str()
                                    .is_ok_and(|len| len.parse::<u32>().is_ok_and(|l| l > 0))
                            })
                        }))
                        .to(entry_point),
                )
                .route("/", web::to(HttpResponse::ImATeapot))
        })
        .shutdown_timeout(5)
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
    UpdateToNewSystem { system_id: String },
}

async fn server_task(
    input_rx: mpsc::Receiver<ServerRequest>,
    state_keeper: StartedStateKeeper,
) -> anyhow::Result<()> {
    let mut input_stream = ReceiverStream::new(input_rx);

    while let Some(req) = input_stream.next().await {
        match req {
            ServerRequest::UpdateToNewSystem { system_id } => {}
        }
    }

    Ok(())
}

async fn entry_point(
    req: HttpRequest,
    payload_string: String,
    inputs_sender: web::Data<mpsc::Sender<ServerRequest>>,
) -> actix_web::Result<impl Responder> {
    // TODO: properly parse the payload string.

    inputs_sender
        .send(ServerRequest::UpdateToNewSystem {
            system_id: payload_string,
        })
        .await
        .map_err(|err| InternalError::new(err, StatusCode::INTERNAL_SERVER_ERROR))?;

    // let download_results = downloader
    //     .download_paths(paths)
    //     .await
    //     .map_err(|err| InternalError::new(err, StatusCode::INTERNAL_SERVER_ERROR))?;

    // unpacker
    //     .unpack_downloads(download_results)
    //     .await
    //     .map_err(|err| InternalError::new(err, StatusCode::INTERNAL_SERVER_ERROR))?;

    Ok(HttpResponse::NoContent())
}
