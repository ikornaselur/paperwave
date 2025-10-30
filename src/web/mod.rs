#![cfg(all(target_os = "linux", feature = "web"))]

use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::Router;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::{fmt, EnvFilter};

mod state;
mod templates;
mod util;
mod handlers;
use state::AppState;

pub fn run_server(host: String, port: u16, probe: paperwave::ProbeInfo) -> paperwave::Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tower_http=info,axum::rejection=trace"));
    let _ = fmt().with_env_filter(filter).with_target(false).try_init();

    let (width, height) = match probe.display {
        Some(paperwave::DisplaySpec::El133Uf1 { width, height }) => (width, height),
        Some(paperwave::DisplaySpec::Uc8159 { width, height, .. }) => (width, height),
        None => {
            let cfg = paperwave::InkyUc8159Config::default();
            (cfg.width, cfg.height)
        }
    };
    let aspect = width as f32 / height as f32;

    let state = AppState {
        width,
        height,
        aspect,
        busy: Arc::new(Mutex::new(())),
        probe: Arc::new(probe),
    };

    let app = Router::new()
        .route("/", get(handlers::index))
        .route("/info", get(handlers::info))
        .route("/status", get(handlers::status))
        .route("/upload", post(handlers::upload))
        .route("/calibrate/start", post(handlers::calibrate_start))
        .route("/calibrate/answer", post(handlers::calibrate_answer))
        .with_state(state)
        .layer(DefaultBodyLimit::max(25 * 1024 * 1024))
        .layer(TraceLayer::new_for_http());

    let addr: SocketAddr = format!("{}:{}", host, port)
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("bind addr parse error: {e}")))?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("tokio runtime error: {e}")))?;

    info!(width, height, aspect = format!("{aspect:.3}"), "Detected panel");

    Ok(rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("bind error: {e}")))?;
        if let Ok(l) = listener.local_addr() {
            info!(address = %format!("http://{l}"), "Listening");
        }
        axum::serve(listener, app)
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("server error: {e}")))
    })?)
}
