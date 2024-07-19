use std::net::IpAddr;

use anyhow::anyhow;
use derive_builder::Builder;
use foundations::telemetry::{
    init_with_server,
    settings::{
        MemoryProfilerSettings, MetricsSettings, TelemetryServerSettings, TelemetrySettings,
    },
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Builder)]
#[builder(pattern = "owned", build_fn(private, name = "build"))]
pub struct TelemetryServer {
    address: IpAddr,
    port: u16,
}

impl TelemetryServer {
    pub fn builder() -> TelemetryServerBuilder {
        Default::default()
    }
}

pub struct StartedTelemetryServer {
    server_task: JoinHandle<anyhow::Result<()>>,
    shutdown_token: CancellationToken,
}

impl StartedTelemetryServer {
    pub async fn shutdown(self) -> anyhow::Result<()> {
        tracing::info!(
            "Telemetry server got a request to shutdown. Proceeding with graceful shutdown."
        );

        self.shutdown_token.cancel();
        self.server_task.await?
    }
}

impl TelemetryServerBuilder {
    pub fn start(self) -> anyhow::Result<StartedTelemetryServer> {
        let server_info = self.build()?;

        let service_info = foundations::service_info!();
        let telemetry_server = init_with_server(
            &service_info,
            &telemetry_server_settings(server_info),
            Vec::new(),
        )?;

        if let Some(addr) = telemetry_server.server_addr() {
            tracing::info!(%addr, "Telemetry server has started.");
        } else {
            return Err(anyhow!("telemetry server was unable to bind to an address"));
        }

        let shutdown_token = CancellationToken::new();

        let cancel_future = shutdown_token.child_token().cancelled_owned();
        let server_task =
            tokio::spawn(
                async move { telemetry_server.with_graceful_shutdown(cancel_future).await },
            );

        Ok(StartedTelemetryServer {
            server_task,
            shutdown_token,
        })
    }
}

fn telemetry_server_settings(info: TelemetryServer) -> TelemetrySettings {
    let mut metrics = MetricsSettings::default();
    metrics.report_optional = true;

    let mut memory_profiler = MemoryProfilerSettings::default();
    memory_profiler.enabled = true;

    TelemetrySettings {
        metrics,
        memory_profiler,
        server: TelemetryServerSettings {
            enabled: true,
            addr: (info.address, info.port).into(),
        },
    }
}
