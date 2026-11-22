//! CLI entrypoint for `waddle-operator`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use kube::{Client, CustomResourceExt};
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;
use waddle_operator::{
    health, run_controller, Cli, Command, HealthState, LogFormat, OperatorError, OperatorMetrics,
};
use waddle_operator_crd::WaddleServer;

#[tokio::main]
async fn main() -> ExitCode {
    if rustls::crypto::ring::default_provider()
        .install_default()
        .is_err()
    {
        // Only fails if another provider was already installed in this process.
    }

    let cli = Cli::parse();
    match cli.command {
        Command::Crd => match render_crd_yaml() {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "failed to render WaddleServer CRD: {err}"
                );
                ExitCode::FAILURE
            }
        },
        Command::Run(args) => {
            init_tracing(args.log_format);
            match run(args).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    tracing::error!(error = %err, "waddle-operator exited with error");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

fn render_crd_yaml() -> Result<(), OperatorError> {
    let crd = WaddleServer::crd();
    let yaml = serde_yaml_ng::to_string(&crd)
        .map_err(|err| OperatorError::Serialization(err.to_string()))?;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(yaml.as_bytes())
        .map_err(OperatorError::HttpServer)?;
    Ok(())
}

async fn run(args: waddle_operator::RunArgs) -> Result<(), OperatorError> {
    let client = Client::try_default()
        .await
        .map_err(|source| OperatorError::Kube {
            context: "initialize Kubernetes client",
            source,
        })?;

    let metrics = Arc::new(OperatorMetrics::new());
    let health_state = Arc::new(HealthState::new(Arc::clone(&metrics)));
    let (ctrl_shutdown_tx, ctrl_shutdown_rx) = watch::channel(false);
    let (probe_shutdown_tx, mut probe_shutdown_rx) = watch::channel(false);

    let sig_tx = ctrl_shutdown_tx.clone();
    tokio::spawn(async move {
        wait_for_os_signal().await;
        tracing::info!("received termination signal; shutting down waddle-operator");
        let _ = sig_tx.send(true);
    });

    let probe_health = Arc::clone(&health_state);
    let probe_bind = args.metrics_bind;
    let mut probe_task = tokio::spawn(async move {
        health::serve_probes(probe_bind, probe_health, async move {
            while !*probe_shutdown_rx.borrow() {
                if probe_shutdown_rx.changed().await.is_err() {
                    break;
                }
            }
        })
        .await
    });

    let ctrl_fut = run_controller(client, args, health_state, metrics, ctrl_shutdown_rx);
    tokio::pin!(ctrl_fut);

    tokio::select! {
        ctrl_res = &mut ctrl_fut => {
            let _ = probe_shutdown_tx.send(true);
            let _ = probe_task.await;
            ctrl_res
        }
        probe_res = &mut probe_task => {
            let _ = ctrl_shutdown_tx.send(true);
            let ctrl_res = ctrl_fut.await;
            match probe_res {
                Ok(Ok(())) => ctrl_res,
                Ok(Err(err)) => Err(err),
                Err(join_err) => Err(OperatorError::InvalidClusterState(format!(
                    "probe task exited unexpectedly: {join_err}"
                ))),
            }
        }
    }
}

async fn wait_for_os_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sigterm) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sigterm.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

fn init_tracing(format: LogFormat) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    match format {
        LogFormat::Json => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .json()
                .with_current_span(false)
                .init();
        }
        LogFormat::Pretty => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .pretty()
                .init();
        }
    }
}
