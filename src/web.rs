#![cfg(target_os = "linux")]

use axum::extract::{DefaultBodyLimit, Multipart};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use image::{imageops, DynamicImage, Rgb, RgbImage};
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use paperwave::InkyDisplay;
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};
use tracing_subscriber::{fmt, EnvFilter};
use serde::{Deserialize};
use std::fs;
use std::path::PathBuf;

#[derive(Clone)]
struct AppState {
    width: u16,
    height: u16,
    aspect: f32,
    busy: Arc<Mutex<()>>, // lock while an update is in progress
    probe: Arc<paperwave::ProbeInfo>,
    rotation_override: Arc<Mutex<Option<paperwave::Rotation>>>,
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

#[derive(Deserialize)]
struct CalibrateAnswerReq { direction: String }

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
        rotation_override: Arc::new(Mutex::new(load_saved_rotation())),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/info", get(info))
        .route("/status", get(status))
        .route("/upload", post(upload))
        .route("/calibrate/start", post(calibrate_start))
        .route("/calibrate/answer", post(calibrate_answer))
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
    .dropzone {{ position: relative; width: min(100%, 560px); aspect-ratio: var(--ar); border: 2px dashed #999; border-radius: 8px; color: #555; background: #fafafa; display: flex; align-items: center; justify-content: center; overflow: hidden; cursor: pointer; }}
    .dropzone.dragover {{ border-color: #2a7; color: #2a7; background: #f6fff9; }}
    .controls {{ margin-top: 1rem; }}
    button {{ padding: 0.6rem 1.2rem; font-size: 1rem; }}
    .muted {{ color: #888; font-size: 0.9rem; }}
    #status {{ margin-top: 0.5rem; min-height: 1.2rem; }}
    #preview {{ width: 100%; height: 100%; display: none; }}
    .placeholder {{ color: #aaa; }}
    .badge {{ position: absolute; top: 8px; left: 8px; background: rgba(0,0,0,0.6); color: #fff; padding: 2px 6px; border-radius: 4px; font-size: 12px; }}
  </style>
</head>
<body>
  <div class="info">
    <strong>Detected panel:</strong> {w} × {h} (aspect {aspect:.3})
  </div>
  <div id="drop" class="dropzone" aria-label="dropzone" style="--ar: {w}/{h}">
    <div id="badge" class="badge">Aspect {w}:{h} (~{aspect:.3})</div>
    <span id="placeholder" class="placeholder">Drag & drop an image, or click</span>
    <canvas id="preview"></canvas>
    <input id="file" type="file" accept="image/*" style="display:none" />
  </div>
  <div class="controls">
    <div class="muted" style="margin-bottom:0.5rem">Orientation controls rotate the display preview only.</div>
    <div class="controls-row" style="display:flex; gap:0.5rem; align-items:center; flex-wrap:wrap; margin-bottom:0.5rem;">
      <span class="muted">Orientation:</span>
      <button id="orient-land" type="button">Landscape</button>
      <button id="orient-port" type="button">Portrait</button>
      <button id="orient-flip" type="button">Flip 180°</button>
    </div>
    <div class="controls-row" style="display:flex; gap:0.75rem; align-items:center; flex-wrap:wrap; margin-bottom:0.5rem;">
      <label class="muted">Rotate
        <select id="rotate" style="margin-left:0.4rem">
          <option value="0">0°</option>
          <option value="90">90°</option>
          <option value="180">180°</option>
          <option value="270">270°</option>
        </select>
      </label>
      <label class="muted">Saturation
        <input id="saturation" type="range" min="0" max="1" step="0.01" value="0.5" style="vertical-align:middle; margin-left:0.4rem" />
        <span id="sat-val">0.50</span>
      </label>
      <label class="muted">Lighten
        <input id="lighten" type="range" min="0" max="1" step="0.01" value="0" style="vertical-align:middle; margin-left:0.4rem" />
        <span id="light-val">0.00</span>
      </label>
    </div>
    <div id="calib-row1" class="controls-row" style="display:flex; gap:0.5rem; align-items:center; flex-wrap:wrap; margin-bottom:0.5rem;">
      <button id="calib-start" type="button">Calibrate orientation</button>
    </div>
    <div id="calib-row2" class="controls-row" style="display:none; gap:0.5rem; align-items:center; flex-wrap:wrap; margin-bottom:0.5rem;">
      <span class="muted">Arrow points:</span>
      <button id="calib-up" type="button">Up</button>
      <button id="calib-right" type="button">Right</button>
      <button id="calib-down" type="button">Down</button>
      <button id="calib-left" type="button">Left</button>
    </div>
    <button id="send" {disabled}>Send to display</button>
    <div id="hint" class="muted">{hint}</div>
    <div id="status"></div>
  </div>
  <script>
    const drop = document.getElementById('drop');
    const fileInput = document.getElementById('file');
    const sendBtn = document.getElementById('send');
    const statusEl = document.getElementById('status');
    const previewEl = document.getElementById('preview');
    const ctx = previewEl.getContext ? previewEl.getContext('2d') : null;
    const placeholderEl = document.getElementById('placeholder');
    const badgeEl = document.getElementById('badge');
    const orientLandBtn = document.getElementById('orient-land');
    const orientPortBtn = document.getElementById('orient-port');
    const orientFlipBtn = document.getElementById('orient-flip');
    const hintEl = document.getElementById('hint');
    const rotateSel = document.getElementById('rotate');
    const satInput = document.getElementById('saturation');
    const lightInput = document.getElementById('lighten');
    const satVal = document.getElementById('sat-val');
    const lightVal = document.getElementById('light-val');
    const calibStartBtn = document.getElementById('calib-start');
    const calibRow1 = document.getElementById('calib-row1');
    const calibRow2 = document.getElementById('calib-row2');
    const calibUp = document.getElementById('calib-up');
    const calibRight = document.getElementById('calib-right');
    const calibDown = document.getElementById('calib-down');
    const calibLeft = document.getElementById('calib-left');
    let file = null;
    let previewUrl = null;
    let currentImage = null;
    let orientation = 'landscape';
    let flipped = false;
    let userRotation = 0; // 0, 90, 180, 270

    const PANEL_W = {w};
    const PANEL_H = {h};
    const ASPECT = {aspect:.6};
    const DEFAULT_PORTRAIT = {default_portrait};

    function setBusy(b) {{
      sendBtn.disabled = b || !file;
      hintEl.textContent = b ? 'Update in progress, please wait…' : (file ? file.name : '');
    }}

    function showPreview(f) {{
      if (!f) {{
        if (previewUrl) {{ URL.revokeObjectURL(previewUrl); previewUrl = null; }}
        previewEl.style.display = 'none';
        if (placeholderEl) placeholderEl.style.display = 'block';
        return;
      }}
      if (previewUrl) {{ URL.revokeObjectURL(previewUrl); previewUrl = null; }}
      previewUrl = URL.createObjectURL(f);
      const imgEl = new Image();
      imgEl.onload = () => {{
        currentImage = imgEl;
        if (placeholderEl) placeholderEl.style.display = 'none';
        previewEl.style.display = 'block';
        renderPreview();
      }};
      imgEl.src = previewUrl;
    }}

    function updateBadge() {{
      if (!badgeEl) return;
      if (orientation === 'landscape') {{
        badgeEl.textContent = `Aspect ${{PANEL_W}}:${{PANEL_H}} (~${{ASPECT.toFixed(3)}})`;
      }} else {{
        badgeEl.textContent = `Aspect ${{PANEL_H}}:${{PANEL_W}} (~${{(1/ASPECT).toFixed(3)}})`;
      }}
    }}

    function setOrientation(mode) {{
      orientation = mode;
      if (mode === 'landscape') {{
        drop.style.setProperty('--ar', `${{PANEL_W}}/${{PANEL_H}}`);
        orientLandBtn.classList.add('active');
        orientPortBtn.classList.remove('active');
      }} else {{
        drop.style.setProperty('--ar', `${{PANEL_H}}/${{PANEL_W}}`);
        orientPortBtn.classList.add('active');
        orientLandBtn.classList.remove('active');
      }}
      updateBadge();
      // Re-render to match new box size
      setTimeout(renderPreview, 0);
    }}

    function setFlipped(value) {{
      flipped = value;
      if (flipped) {{ orientFlipBtn.classList.add('active'); }} else {{ orientFlipBtn.classList.remove('active'); }}
      updatePreviewTransform();
    }}

    function renderPreview() {{
      if (!ctx || !currentImage) return;
      const total = (userRotation + (flipped ? 180 : 0)) % 360;
      const rad = total * Math.PI / 180;
      const box = drop.getBoundingClientRect();
      const cw = Math.max(1, Math.floor(box.width));
      const ch = Math.max(1, Math.floor(box.height));
      previewEl.width = cw;
      previewEl.height = ch;
      ctx.reset && ctx.reset();
      ctx.clearRect(0,0,cw,ch);
      // Filters
      const s = parseFloat(satInput.value) || 0;
      const l = parseFloat(lightInput.value) || 0;
      const brightness = (1.0 + 0.30 * l).toFixed(2);
      ctx.filter = `saturate(${{s.toFixed(2)}}) brightness(${{brightness}})`;
      // Center and rotate
      ctx.save();
      ctx.translate(cw/2, ch/2);
      ctx.rotate(rad);
      const iw = currentImage.naturalWidth;
      const ih = currentImage.naturalHeight;
      const swap = (total % 180) !== 0;
      const dw = swap ? ih : iw;
      const dh = swap ? iw : ih;
      const scale = Math.max(cw / dw, ch / dh);
      const drawW = iw * scale;
      const drawH = ih * scale;
      ctx.drawImage(currentImage, -drawW/2, -drawH/2, drawW, drawH);
      ctx.restore();
    }}

    drop.addEventListener('click', () => fileInput.click());
    drop.addEventListener('dragover', e => {{ e.preventDefault(); drop.classList.add('dragover'); }});
    drop.addEventListener('dragleave', e => {{ drop.classList.remove('dragover'); }});
    drop.addEventListener('drop', e => {{
      e.preventDefault(); drop.classList.remove('dragover');
      const f = e.dataTransfer.files[0];
      if (f) {{ file = f; hintEl.textContent = f.name; sendBtn.disabled = false; showPreview(file); }}
    }});
    fileInput.addEventListener('change', e => {{
      const f = e.target.files[0];
      if (f) {{ file = f; hintEl.textContent = f.name; sendBtn.disabled = false; showPreview(file); }}
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

    // Orientation controls
    orientLandBtn.addEventListener('click', () => setOrientation('landscape'));
    orientPortBtn.addEventListener('click', () => setOrientation('portrait'));
    orientFlipBtn.addEventListener('click', () => setFlipped(!flipped));
    setOrientation(DEFAULT_PORTRAIT ? 'portrait' : 'landscape');
    setFlipped(false);
    renderPreview();

    rotateSel.addEventListener('change', () => {{
      userRotation = parseInt(rotateSel.value, 10) || 0;
      renderPreview();
    }});
    function updatePreviewFilters() {{
      const s = parseFloat(satInput.value) || 0;
      const l = parseFloat(lightInput.value) || 0;
      const brightness = (1.0 + 0.30 * l).toFixed(2);
      previewEl.style.filter = 'saturate(' + s.toFixed(2) + ') brightness(' + brightness + ')';
    }}
    function updateSat() {{ satVal.textContent = parseFloat(satInput.value).toFixed(2); updatePreviewFilters(); }}
    function updateLight() {{ lightVal.textContent = parseFloat(lightInput.value).toFixed(2); updatePreviewFilters(); }}
    satInput.addEventListener('input', updateSat); updateSat();
    lightInput.addEventListener('input', updateLight); updateLight();

    // Calibration flow
    async function startCalibration() {{
      calibStartBtn.disabled = true;
      statusEl.textContent = 'Drawing calibration arrow…';
      try {{
        const r = await fetch('/calibrate/start', {{ method: 'POST' }});
        if (r.status === 423) {{
          statusEl.textContent = 'Display busy — try calibration again later.';
          calibStartBtn.disabled = false;
          return;
        }}
        if (!r.ok) {{
          const t = await r.text();
          statusEl.textContent = 'Calibration error: ' + t;
          calibStartBtn.disabled = false;
          return;
        }}
        statusEl.textContent = 'Check the device. Which way does the arrow point?';
        calibRow2.style.display = 'flex';
      }} catch (e) {{
        statusEl.textContent = 'Calibration start failed';
        calibStartBtn.disabled = false;
      }}
    }}

    async function sendCalibration(direction) {{
      statusEl.textContent = 'Saving calibration…';
      try {{
        const r = await fetch('/calibrate/answer', {{
          method: 'POST',
          headers: {{ 'Content-Type': 'application/json' }},
          body: JSON.stringify({{ direction }})
        }});
        if (!r.ok) {{
          const t = await r.text();
          statusEl.textContent = 'Calibration error: ' + t;
          return;
        }}
        statusEl.textContent = 'Calibration saved. Future uploads will use this rotation.';
        calibRow2.style.display = 'none';
        calibStartBtn.disabled = false;
      }} catch (e) {{
        statusEl.textContent = 'Calibration save failed';
      }}
    }}

    calibStartBtn.addEventListener('click', startCalibration);
    calibUp.addEventListener('click', () => sendCalibration('up'));
    calibRight.addEventListener('click', () => sendCalibration('right'));
    calibDown.addEventListener('click', () => sendCalibration('down'));
    calibLeft.addEventListener('click', () => sendCalibration('left'));

    sendBtn.addEventListener('click', async () => {{
      if (!file) return;
      sendBtn.disabled = true;
      statusEl.textContent = 'Uploading…';
      const fd = new FormData();
      fd.append('file', file);
      fd.append('rotation', String(userRotation));
      fd.append('saturation', satInput.value);
      fd.append('lighten', lightInput.value);
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
        default_portrait = match state.clone().probe.display { Some(paperwave::DisplaySpec::El133Uf1{..}) => true, _ => false },
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

async fn calibrate_start(state: axum::extract::State<AppState>) -> impl IntoResponse {
    let guard = match state.busy.try_lock() {
        Ok(g) => g,
        Err(_) => return StatusCode::LOCKED.into_response(),
    };
    let probe = state.probe.clone();
    info!("Starting calibration: drawing UP arrow");
    let res = tokio::task::spawn_blocking(move || {
        let rotation = paperwave::Rotation::Deg0;
        let mut display = create_display_from_probe(rotation, &probe)?;
        let img = arrow_image(display.input_dimensions(), 'U');
        display.set_image(&img, 0.5, 0.0)?;
        display.show()
    })
    .await
    .map_err(|e| format!("task join error: {e}"))
    .and_then(|r| r.map_err(|e| format!("{e}")));
    drop(guard);
    match res {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn calibrate_answer(
    state: axum::extract::State<AppState>,
    Json(req): Json<CalibrateAnswerReq>,
) -> impl IntoResponse {
    let dir = req.direction.to_lowercase();
    let rotation = match dir.as_str() {
        // If UP was displayed but user saw it as RIGHT, we need to rotate -90 (Deg270)
        "up" => paperwave::Rotation::Deg0,
        "right" => paperwave::Rotation::Deg270,
        "down" => paperwave::Rotation::Deg180,
        "left" => paperwave::Rotation::Deg90,
        _ => return (StatusCode::BAD_REQUEST, "invalid direction").into_response(),
    };

    // Save to disk and refresh in-memory cache
    if let Err(e) = save_rotation(rotation) {
        warn!(error = %e, "Failed saving rotation");
    }
    {
        let mut lock = state.rotation_override.lock().await;
        *lock = Some(rotation);
    }
    info!(?rotation, "Calibration saved");
    StatusCode::OK.into_response()
}

fn arrow_image(dim: (u16, u16), dir: char) -> DynamicImage {
    let (w, h) = (dim.0 as u32, dim.1 as u32);
    let mut img = RgbImage::from_pixel(w, h, Rgb([255, 255, 255]));
    let cx = (w / 2) as i32;
    let tip_y = (h as f32 * 0.12) as i32;
    let base_y = (h as f32 * 0.62) as i32;
    let max_half = ((w as f32) * 0.35) as i32;
    let red = Rgb([200, 20, 20]);

    for y in tip_y..=base_y {
        let t = (y - tip_y) as f32 / (base_y - tip_y).max(1) as f32;
        let half = (max_half as f32 * t) as i32;
        let x0 = (cx - half).max(0) as u32;
        let x1 = (cx + half).min(w as i32 - 1) as u32;
        for x in x0..=x1 {
            img.put_pixel(x, y as u32, red);
        }
    }
    // Shaft
    let shaft_w = (w as f32 * 0.10).max(1.0) as i32;
    let shaft_y0 = base_y;
    let shaft_y1 = (h as f32 * 0.90) as i32;
    let x0 = (cx - shaft_w / 2).max(0) as u32;
    let x1 = (cx + shaft_w / 2).min(w as i32 - 1) as u32;
    for y in shaft_y0..=shaft_y1 {
        for x in x0..=x1 {
            img.put_pixel(x, y as u32, red);
        }
    }

    let r#dyn = DynamicImage::ImageRgb8(img);
    match dir {
        'U' => r#dyn,
        'R' => DynamicImage::ImageRgba8(imageops::rotate90(&r#dyn)),
        'D' => DynamicImage::ImageRgba8(imageops::rotate180(&r#dyn)),
        'L' => DynamicImage::ImageRgba8(imageops::rotate270(&r#dyn)),
        _ => r#dyn,
    }
}

fn config_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".config/paperwave");
        let _ = fs::create_dir_all(&p);
        p.push("state.json");
        return p;
    }
    PathBuf::from("paperwave_state.json")
}

fn load_saved_rotation() -> Option<paperwave::Rotation> {
    #[derive(Deserialize)]
    struct State { rotation_deg: u16 }
    let path = config_path();
    let data = fs::read(path).ok()?;
    let st: State = serde_json::from_slice(&data).ok()?;
    match st.rotation_deg % 360 {
        0 => Some(paperwave::Rotation::Deg0),
        90 => Some(paperwave::Rotation::Deg90),
        180 => Some(paperwave::Rotation::Deg180),
        270 => Some(paperwave::Rotation::Deg270),
        _ => None,
    }
}

fn save_rotation(rot: paperwave::Rotation) -> std::io::Result<()> {
    #[derive(serde::Serialize)]
    struct State { rotation_deg: u16 }
    let deg = match rot {
        paperwave::Rotation::Deg0 => 0,
        paperwave::Rotation::Deg90 => 90,
        paperwave::Rotation::Deg180 => 180,
        paperwave::Rotation::Deg270 => 270,
    };
    let path = config_path();
    let data = serde_json::to_vec_pretty(&State { rotation_deg: deg }).unwrap();
    fs::write(path, data)
}

fn apply_exif_orientation(bytes: &[u8], img: DynamicImage) -> DynamicImage {
    match exif_orientation_from_jpeg(bytes) {
        Some(90) => DynamicImage::ImageRgba8(imageops::rotate90(&img)),
        Some(180) => DynamicImage::ImageRgba8(imageops::rotate180(&img)),
        Some(270) => DynamicImage::ImageRgba8(imageops::rotate270(&img)),
        _ => img,
    }
}

// Minimal EXIF parser for JPEG APP1 Exif orientation (tag 0x0112)
fn exif_orientation_from_jpeg(bytes: &[u8]) -> Option<u16> {
    // JPEG signature
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 { return None; }
    let mut i = 2usize;
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xFF { i += 1; continue; }
        let marker = bytes[i+1];
        i += 2;
        if marker == 0xD9 || marker == 0xDA { break; } // EOI or SOS
        if i + 2 > bytes.len() { break; }
        let seg_len = u16::from_be_bytes([bytes[i], bytes[i+1]]) as usize;
        i += 2;
        if seg_len < 2 || i + seg_len - 2 > bytes.len() { break; }
        if marker == 0xE1 { // APP1
            let data = &bytes[i..i + seg_len - 2];
            if data.len() >= 6 && &data[0..6] == b"Exif\0\0" {
                // TIFF header after 6 bytes
                return parse_tiff_orientation(&data[6..]);
            }
        }
        i += seg_len - 2;
    }
    None
}

fn parse_tiff_orientation(tiff: &[u8]) -> Option<u16> {
    if tiff.len() < 8 { return None; }
    let be = if &tiff[0..2] == b"MM" { true } else if &tiff[0..2] == b"II" { false } else { return None; };
    let u16_at = |off: usize, be: bool| -> Option<u16> {
        if off + 2 > tiff.len() { return None; }
        Some(if be { u16::from_be_bytes([tiff[off], tiff[off+1]]) } else { u16::from_le_bytes([tiff[off], tiff[off+1]]) })
    };
    let u32_at = |off: usize, be: bool| -> Option<u32> {
        if off + 4 > tiff.len() { return None; }
        Some(if be { u32::from_be_bytes([tiff[off], tiff[off+1], tiff[off+2], tiff[off+3]]) } else { u32::from_le_bytes([tiff[off], tiff[off+1], tiff[off+2], tiff[off+3]]) })
    };
    if u16_at(2, be)? != 0x002A { return None; }
    let ifd0_off = u32_at(4, be)? as usize;
    if ifd0_off + 2 > tiff.len() { return None; }
    let count = u16_at(ifd0_off, be)? as usize;
    let mut p = ifd0_off + 2;
    for _ in 0..count {
        if p + 12 > tiff.len() { return None; }
        let tag = u16_at(p, be)?;
        let typ = u16_at(p+2, be)?;
        let cnt = u32_at(p+4, be)?;
        let val_off = u32_at(p+8, be)? as usize;
        if tag == 0x0112 { // Orientation
            if typ == 3 && cnt == 1 { // SHORT
                let value = if be { (val_off >> 16) as u16 } else { (val_off & 0xFFFF) as u16 };
                return match value { 3=>Some(180), 6=>Some(90), 8=>Some(270), _=>None };
            } else if typ == 3 && cnt >= 1 {
                // Value at offset
                let off = val_off;
                if off + 2 <= tiff.len() {
                    let v = if be { u16::from_be_bytes([tiff[off], tiff[off+1]]) } else { u16::from_le_bytes([tiff[off], tiff[off+1]]) };
                    return match v { 3=>Some(180), 6=>Some(90), 8=>Some(270), _=>None };
                }
            }
            return None;
        }
        p += 12;
    }
    None
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

    // Find first file part named 'file' and optional params
    let mut bytes: Option<Vec<u8>> = None;
    let mut saturation: f32 = 0.5;
    let mut lighten: f32 = 0.0;
    let mut rotation_deg: u16 = 0;
    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                if let Some(name) = field.name() {
                    match name {
                        "file" => {
                            match field.bytes().await {
                                Ok(b) => {
                                    info!(size = b.len(), "Received upload");
                                    bytes = Some(b.to_vec());
                                }
                                Err(e) => {
                                    warn!(error = %e, "Failed reading upload field");
                                    drop(guard);
                                    return (StatusCode::BAD_REQUEST, format!("read error: {e}")).into_response();
                                }
                            }
                        }
                        "saturation" => {
                            if let Ok(val) = field.text().await { if let Ok(v) = val.parse::<f32>() { saturation = v.clamp(0.0, 1.0); } }
                        }
                        "lighten" => {
                            if let Ok(val) = field.text().await { if let Ok(v) = val.parse::<f32>() { lighten = v.clamp(0.0, 1.0); } }
                        }
                        "rotation" => {
                            if let Ok(val) = field.text().await { if let Ok(v) = val.parse::<u16>() { rotation_deg = match v % 360 {0=>0,90=>90,180=>180,270=>270,_=>0}; } }
                        }
                        _ => {}
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
    // Apply EXIF orientation if present (JPEGs etc.) so device matches browser preview
    let img = apply_exif_orientation(&buf, img);

    // Process update in a blocking task; keep the lock held so others are rejected.
    let probe = state.probe.clone();
    let rotation_override = { state.rotation_override.lock().await.clone() };
    let rotation_deg = rotation_deg;
    let saturation = saturation;
    let lighten = lighten;
    info!("Starting display update");
    let res = tokio::task::spawn_blocking(move || update_display(&probe, &img, rotation_override, rotation_deg, saturation, lighten))
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

fn update_display(
    probe: &paperwave::ProbeInfo,
    image: &DynamicImage,
    rotation_override: Option<paperwave::Rotation>,
    user_rotation_deg: u16,
    saturation: f32,
    lighten: f32,
) -> paperwave::Result<()> {
    // Defaults aligned with CLI: rotation 0, saturation 0.5, lighten 0.0
    let rotation = combine_rotation(rotation_override, user_rotation_deg);
    let mut display = create_display_from_probe(rotation, probe)?;
    display.set_image(image, saturation, lighten)?;
    display.show()
}

fn combine_rotation(calibrated: Option<paperwave::Rotation>, user_deg: u16) -> paperwave::Rotation {
    let base_deg = match calibrated.unwrap_or(paperwave::Rotation::Deg0) {
        paperwave::Rotation::Deg0 => 0u16,
        paperwave::Rotation::Deg90 => 90,
        paperwave::Rotation::Deg180 => 180,
        paperwave::Rotation::Deg270 => 270,
    };
    let total = (base_deg + match user_deg % 360 { 0=>0, 90=>90, 180=>180, 270=>270, _=>0 }) % 360;
    match total { 0=>paperwave::Rotation::Deg0, 90=>paperwave::Rotation::Deg90, 180=>paperwave::Rotation::Deg180, 270=>paperwave::Rotation::Deg270, _=>paperwave::Rotation::Deg0 }
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
