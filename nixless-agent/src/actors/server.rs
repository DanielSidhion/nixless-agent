use std::collections::HashSet;

use actix_web::{
    error::InternalError, http::StatusCode, web, App, HttpRequest, HttpResponse, HttpServer,
    Responder,
};
use anyhow::anyhow;
use derive_builder::Builder;
use nix_core::{NixStylePublicKey, PublicKeychain};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tracing::instrument;

use super::StartedStateKeeper;

#[derive(Builder)]
#[builder(pattern = "owned")]
pub struct Server {
    port: u16,
    state_keeper: StartedStateKeeper,
    update_public_key: String,
}

impl Server {
    pub fn builder() -> ServerBuilder {
        ServerBuilder::default()
    }

    pub fn start(self) -> anyhow::Result<StartedServer> {
        let mut keychain = PublicKeychain::new();
        let public_key = NixStylePublicKey::from_nix_format(&self.update_public_key)?;
        keychain.add_key(public_key)?;

        let (input_tx, input_rx) = mpsc::channel(10);

        let inputs_task = tokio::spawn(server_task(input_rx, self.state_keeper));

        let inputs_sender = web::Data::new(input_tx.clone());
        let keychain = web::Data::new(keychain);
        let server_task = HttpServer::new(move || {
            App::new()
                .app_data(inputs_sender.clone())
                .app_data(keychain.clone())
                .route(
                    "/new-configuration",
                    web::post().to(handle_new_configuration),
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
    UpdateSystem {
        system_package_id: String,
        package_ids: HashSet<String>,
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
            ServerRequest::UpdateSystem {
                system_package_id,
                package_ids,
                resp_tx,
            } => {
                let res = state_keeper
                    .switch_to_new_configuration(system_package_id, package_ids)
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
async fn handle_new_configuration(
    req: HttpRequest,
    payload_string: String,
    inputs_sender: web::Data<mpsc::Sender<ServerRequest>>,
    keychain: web::Data<PublicKeychain>,
) -> actix_web::Result<impl Responder> {
    let mut lines = payload_string.lines();

    if let Some(system_package_id) = lines.next() {
        tracing::info!(system_package_id, "Got a new system configuration request!");

        // A bit convoluted since we first need to grab the last line (which is the signature) and remove it from the list of package ids, and only then turn the list into a set.
        let mut package_ids: Vec<_> = lines.map(str::to_string).collect();
        let signature = package_ids.pop();
        package_ids.push(system_package_id.to_string());
        let package_ids = HashSet::from_iter(package_ids.into_iter());

        let Some(signature) = signature else {
            tracing::info!("Request didn't have a signature included!");
            return Ok(HttpResponse::BadRequest().finish());
        };

        let signed_data = payload_string.trim().trim_end_matches(&signature).trim();
        let signature_ok = keychain
            .verify_any(signed_data.as_bytes(), signature.as_bytes())
            .map_err(|err| InternalError::new(err, StatusCode::INTERNAL_SERVER_ERROR))?;

        if !signature_ok {
            return Ok(HttpResponse::BadRequest().finish());
        }

        let (resp_tx, resp_rx) = oneshot::channel();

        tracing::info!("Sending server request to update the system.");

        inputs_sender
            .send(ServerRequest::UpdateSystem {
                system_package_id: system_package_id.to_string(),
                package_ids,
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
