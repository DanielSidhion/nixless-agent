use std::{collections::HashSet, net::IpAddr};

use actix_web::{
    dev::ServerHandle, error::InternalError, http::StatusCode, web, App, Either, HttpRequest,
    HttpResponse, HttpServer, Responder,
};
use anyhow::anyhow;
use derive_builder::Builder;
use nix_core::{NixStylePublicKey, PublicKeychain};
use serde_json::json;
use tokio::task::JoinHandle;
use tracing::instrument;

use crate::metrics;

use super::StartedStateKeeperInput;

#[derive(Builder)]
#[builder(pattern = "owned")]
pub struct Server {
    address: IpAddr,
    port: u16,
    state_keeper_input: StartedStateKeeperInput,
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

        let keychain = web::Data::new(keychain);
        let server_task = HttpServer::new(move || {
            App::new()
                .app_data(web::Data::new(self.state_keeper_input.clone()))
                .app_data(keychain.clone())
                .route("/summary", web::get().to(retrieve_system_summary))
                .route(
                    "/new-configuration",
                    web::post().to(handle_new_configuration),
                )
                .route(
                    "/rollback-configuration",
                    web::post().to(rollback_configuration),
                )
                .route("/", web::to(HttpResponse::ImATeapot))
        })
        .disable_signals()
        .shutdown_timeout(5)
        .workers(2)
        .bind((self.address, self.port))?
        .run();

        let server_handle = server_task.handle();
        let server_task = tokio::spawn(async { server_task.await });

        Ok(StartedServer {
            server_task,
            server_handle,
        })
    }
}

pub struct StartedServer {
    server_task: JoinHandle<std::io::Result<()>>,
    server_handle: ServerHandle,
}

impl StartedServer {
    pub async fn shutdown(self) -> anyhow::Result<()> {
        tracing::info!(
            "Control server got a request to shutdown. Proceeding with graceful shutdown."
        );

        self.server_handle.stop(true).await;
        self.server_task
            .await?
            .map_err(|e| anyhow!("control server encountered an error during shutdown: {}", e))
    }
}

#[instrument(skip_all, fields(uri = req.uri().to_string(), method = req.method().as_str()))]
async fn handle_new_configuration(
    req: HttpRequest,
    payload_string: String,
    state_keeper: web::Data<StartedStateKeeperInput>,
    keychain: web::Data<PublicKeychain>,
) -> actix_web::Result<impl Responder> {
    metrics::requests::new_configuration().inc();

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

        tracing::info!("Sending server request to update the system.");

        match state_keeper
            .switch_to_new_configuration(system_package_id.to_string(), package_ids)
            .await
        {
            Ok(()) => Ok(HttpResponse::NoContent().finish()),
            Err(err) => Ok(HttpResponse::Conflict().body(err.to_string())),
        }
    } else {
        Ok(HttpResponse::BadRequest().finish())
    }
}

#[instrument(skip_all)]
async fn retrieve_system_summary(
    state_keeper: web::Data<StartedStateKeeperInput>,
) -> actix_web::Result<impl Responder> {
    metrics::requests::summary().inc();

    match state_keeper.get_summary().await {
        Ok(summary) => {
            let mut resp = json!({
                "current_config": serde_json::to_value(summary.stable_configuration).unwrap(),
                "status": summary.status.as_str(),
            });

            if let Some(extra_config) = summary.status.into_inner_configuration() {
                resp.as_object_mut().unwrap().insert(
                    "outstanding_config".to_string(),
                    serde_json::to_value(extra_config).unwrap(),
                );
            }

            Ok(Either::Left(web::Json(resp)))
        }
        Err(err) => Ok(Either::Right(
            HttpResponse::Conflict().body(err.to_string()),
        )),
    }
}

#[instrument(skip_all)]
async fn rollback_configuration(
    payload_string: String,
    state_keeper: web::Data<StartedStateKeeperInput>,
) -> actix_web::Result<impl Responder> {
    metrics::requests::rollback().inc();

    let version_to_rollback: Option<u32> = if payload_string.is_empty() {
        None
    } else {
        Some(
            payload_string
                .parse()
                .map_err(|err| InternalError::new(err, StatusCode::INTERNAL_SERVER_ERROR))?,
        )
    };

    match state_keeper.perform_rollback(version_to_rollback).await {
        Ok(()) => Ok(HttpResponse::NoContent().finish()),
        Err(err) => Ok(HttpResponse::Conflict().body(err.to_string())),
    }
}
