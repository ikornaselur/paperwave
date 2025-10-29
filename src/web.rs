#![cfg(target_os = "linux")]

use axum::extract::{DefaultBodyLimit, Multipart};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use image::DynamicImage;
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use paperwave::InkyDisplay;
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Clone)]
struct AppState {
    width: u16,
    height: u16,
    aspect: f32,
    busy: Arc<Mutex<()>>, // lock while an update is in progress
    probe: Arc<paperwave::ProbeInfo>,
}

#[derive(Serialize)]
struct InfoResponse {
    width: u16,
    height: u16,
    aspect: f32,
    busy: bool,
}

#[derive(Serialize)]
struct StatusResponse {
    busy: bool,
}

pub fn run_server(host: String, port: u16, probe: paperwave::ProbeInfo) -> paperwave::Result<()> {
    // Initialize logging (honors RUST_LOG if present)
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tower_http=info,axum::rejection=trace"));
    let _ = fmt().with_env_filter(filter).with_target(false).try_init();

    let (width, height) = match probe.display {
        Some(paperwave::DisplaySpec::El133Uf1 { width, height }) => (width, height),
        Some(paperwave::DisplaySpec::Uc8159 { width, height, .. }) => (width, height),
        None => {
            // Fallback to default UC8159 size (matches CLI behavior)
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
        .route("/", get(index))
        .route("/info", get(info))
        .route("/status", get(status))
        .route("/upload", post(upload))
        .with_state(state)
        // Allow reasonably large images by default (25 MiB)
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
        let local = listener.local_addr().ok();
        if let Some(l) = local { info!(address = %format!("http://{l}"), "Listening"); }
        axum::serve(listener, app)
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("server error: {e}")))
    })?)
}

async fn index(state: axum::extract::State<AppState>) -> impl IntoResponse {
    let busy = !state.busy.try_lock().is_ok();
    let html = format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Paperwave Web</title>
  <style>
    body {{ font-family: system-ui, -apple-system, Segoe UI, Roboto, sans-serif; margin: 2rem; }}
    .info {{ margin-bottom: 1rem; color: #333; }}
    .dropzone {{ border: 2px dashed #999; border-radius: 8px; padding: 2rem; text-align: center; color: #555; }}
    .dropzone.dragover {{ border-color: #2a7; color: #2a7; background: #f6fff9; }}
    .controls {{ margin-top: 1rem; }}
    button {{ padding: 0.6rem 1.2rem; font-size: 1rem; }}
    .muted {{ color: #888; font-size: 0.9rem; }}
    #status {{ margin-top: 0.5rem; min-height: 1.2rem; }}
  </style>
</head>
<body>
  <div class="info">
    <strong>Detected panel:</strong> {w} × {h} (aspect {aspect:.3})
  </div>
  <div id="drop" class="dropzone" aria-label="dropzone">
    <p>Drag & drop an image here, or click to choose</p>
    <input id="file" type="file" accept="image/*" style="display:none" />
  </div>
  <div class="controls">
    <button id="send" {disabled}>Send to display</button>
    <div id="hint" class="muted">{hint}</div>
    <div id="status"></div>
  </div>
  <script>
    const drop = document.getElementById('drop');
    const fileInput = document.getElementById('file');
    const sendBtn = document.getElementById('send');
    const statusEl = document.getElementById('status');
    const hintEl = document.getElementById('hint');
    let file = null;

    function setBusy(b) {{
      sendBtn.disabled = b || !file;
      hintEl.textContent = b ? 'Update in progress, please wait…' : (file ? file.name : '');
    }}

    drop.addEventListener('click', () => fileInput.click());
    drop.addEventListener('dragover', e => {{ e.preventDefault(); drop.classList.add('dragover'); }});
    drop.addEventListener('dragleave', e => {{ drop.classList.remove('dragover'); }});
    drop.addEventListener('drop', e => {{
      e.preventDefault(); drop.classList.remove('dragover');
      const f = e.dataTransfer.files[0];
      if (f) {{ file = f; hintEl.textContent = f.name; sendBtn.disabled = false; }}
    }});
    fileInput.addEventListener('change', e => {{
      const f = e.target.files[0];
      if (f) {{ file = f; hintEl.textContent = f.name; sendBtn.disabled = false; }}
    }});

    async function pollStatus() {{
      try {{
        const r = await fetch('/status');
        const j = await r.json();
        setBusy(j.busy);
      }} catch {{ /* ignore */ }}
    }}
    setInterval(pollStatus, 1500);
    pollStatus();

    sendBtn.addEventListener('click', async () => {{
      if (!file) return;
      sendBtn.disabled = true;
      statusEl.textContent = 'Uploading…';
      const fd = new FormData();
      fd.append('file', file);
      try {{
        const r = await fetch('/upload', {{ method: 'POST', body: fd }});
        if (r.status === 423) {{
          statusEl.textContent = 'Display busy — trying again shortly…';
        }} else if (!r.ok) {{
          const t = await r.text();
          statusEl.textContent = 'Error: ' + t;
        }} else {{
          statusEl.textContent = 'Update sent to display.';
        }}
      }} catch (e) {{
        statusEl.textContent = 'Upload failed';
      }} finally {{
        sendBtn.disabled = !file;
      }}
    }});
  </script>
</body>
</html>
"#,
        w = state.width,
        h = state.height,
        aspect = state.aspect,
        disabled = if busy { "disabled" } else { "" },
        hint = if busy { "Update in progress, please wait…" } else { "" },
    );
    Html(html)
}

async fn info(state: axum::extract::State<AppState>) -> impl IntoResponse {
    let busy = !state.busy.try_lock().is_ok();
    Json(InfoResponse {
        width: state.width,
        height: state.height,
        aspect: state.aspect,
        busy,
    })
}

async fn status(state: axum::extract::State<AppState>) -> impl IntoResponse {
    let busy = !state.busy.try_lock().is_ok();
    Json(StatusResponse { busy })
}

async fn upload(
    state: axum::extract::State<AppState>,
    mut multipart: Multipart,
)
-> impl IntoResponse {
    // Try to acquire the busy lock without waiting
    let guard = match state.busy.try_lock() {
        Ok(g) => g,
        Err(_) => {
            warn!("Upload rejected: display busy");
            return StatusCode::LOCKED.into_response();
        }
    };

    // Find first file part named 'file'
    let mut bytes: Option<Vec<u8>> = None;
    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                if let Some(name) = field.name() {
                    if name == "file" {
                        match field.bytes().await {
                            Ok(b) => {
                                info!(size = b.len(), "Received upload");
                                bytes = Some(b.to_vec());
                                break;
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed reading upload field");
                                drop(guard);
                                return (StatusCode::BAD_REQUEST, format!("read error: {e}")).into_response();
                            }
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                warn!(error = %e, "Multipart parse error");
                drop(guard);
                return (StatusCode::BAD_REQUEST, format!("multipart parse error: {e}")).into_response();
            }
        }
    }

    let Some(buf) = bytes else {
        warn!("Upload missing file field");
        drop(guard);
        return (StatusCode::BAD_REQUEST, "no file provided").into_response();
    };

    // Decode image
    let img = match image::load_from_memory(&buf) {
        Ok(i) => i,
        Err(e) => {
            warn!(error = %e, "Invalid image upload");
            drop(guard);
            return (StatusCode::BAD_REQUEST, format!("invalid image: {e}")).into_response();
        }
    };

    // Process update in a blocking task; keep the lock held so others are rejected.
    let probe = state.probe.clone();
    info!("Starting display update");
    let res = tokio::task::spawn_blocking(move || update_display(&probe, &img))
        .await
        .map_err(|e| format!("task join error: {e}"))
        .and_then(|r| r.map_err(|e| format!("{e}")));

    drop(guard);

    match res {
        Ok(()) => {
            info!("Display update complete");
            StatusCode::OK.into_response()
        }
        Err(e) => {
            error!(error = %e, "Display update failed");
            (StatusCode::INTERNAL_SERVER_ERROR, e).into_response()
        }
    }
}

fn update_display(probe: &paperwave::ProbeInfo, image: &DynamicImage) -> paperwave::Result<()> {
    // Defaults aligned with CLI: rotation 0, saturation 0.5, lighten 0.0
    let rotation = paperwave::Rotation::Deg0;
    let mut display = create_display_from_probe(rotation, probe)?;
    display.set_image(image, 0.5, 0.0)?;
    display.show()
}

fn create_display_from_probe(
    rotation: paperwave::Rotation,
    probe: &paperwave::ProbeInfo,
) -> paperwave::Result<Box<dyn paperwave::InkyDisplay>> {
    match probe.display {
        Some(paperwave::DisplaySpec::El133Uf1 { width, height }) => {
            let mut config = paperwave::InkyEl133Uf1Config::default();
            config.width = width;
            config.height = height;
            config.rotation = rotation;
            let mut display = paperwave::InkyEl133Uf1::new(config)?;
            display.set_rotation(rotation);
            Ok(Box::new(display))
        }
        Some(paperwave::DisplaySpec::Uc8159 { width, height, .. }) => {
            let mut config = paperwave::InkyUc8159Config::default();
            config.width = width;
            config.height = height;
            config.rotation = rotation;
            let mut display = paperwave::InkyUc8159::new(config)?;
            display.set_rotation(rotation);
            Ok(Box::new(display))
        }
        None => {
            let mut config = paperwave::InkyUc8159Config::default();
            config.rotation = rotation;
            let mut display = paperwave::InkyUc8159::new(config)?;
            display.set_rotation(rotation);
            Ok(Box::new(display))
        }
    }
}
