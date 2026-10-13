//! rev-proxy: a reverse HTTP proxy built on Tokio + Hyper 1.x.

mod config;
mod proxy;

use std::{sync::Arc, time::Duration};

use hyper::{server::conn::http1, service::service_fn};
use hyper_util::{rt::TokioIo, server::graceful::GracefulShutdown};
use tokio::net::TcpListener;
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

use crate::{config::Config, proxy::Proxy};

/// How long to wait for in-flight requests after a shutdown signal.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let cfg = match Config::from_args() {
        Ok(cfg) => cfg,
        Err(msg) if msg.is_empty() => {
            println!("{}", config::USAGE);
            return Ok(());
        }
        Err(msg) => {
            eprintln!("error: {msg}\n\n{}", config::USAGE);
            std::process::exit(2);
        }
    };

    let proxy = Arc::new(Proxy::new(&cfg));
    let listener = TcpListener::bind(cfg.listen).await?;
    let upstreams: Vec<String> = cfg
        .upstreams
        .iter()
        .map(|u| format!("{}://{}", u.scheme, u.authority))
        .collect();
    info!(listen = %cfg.listen, upstreams = ?upstreams, "reverse proxy started");

    let graceful = GracefulShutdown::new();
    let mut shutdown = std::pin::pin!(shutdown_signal());

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(conn) => conn,
                    Err(e) => {
                        // e.g. EMFILE: back off briefly instead of spinning.
                        warn!(error = %e, "accept failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let _ = stream.set_nodelay(true);

                let proxy = proxy.clone();
                let service = service_fn(move |req| proxy.clone().handle(req, peer));
                let conn = http1::Builder::new()
                    .keep_alive(true)
                    .serve_connection(TokioIo::new(stream), service);
                let conn = graceful.watch(conn);

                tokio::spawn(async move {
                    if let Err(e) = conn.await {
                        debug!(%peer, error = %e, "connection closed with error");
                    }
                });
            }
            _ = &mut shutdown => {
                info!("shutdown signal received, draining connections");
                break;
            }
        }
    }

    drop(listener);
    tokio::select! {
        _ = graceful.shutdown() => info!("all connections drained"),
        _ = tokio::time::sleep(DRAIN_TIMEOUT) => warn!("drain timed out, exiting anyway"),
    }
    Ok(())
}

/// Resolves on Ctrl+C, or SIGTERM on Unix.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
