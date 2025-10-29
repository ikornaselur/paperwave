#![cfg(all(target_os = "linux", feature = "web"))]

use askama::Template;
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use image::DynamicImage;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_http::trace::TraceLayer;
use paperwave::InkyDisplay;
use paperwave::displays::common::apply_exif_orientation_bytes;
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

#[derive(Deserialize)]
struct CalibrateAnswerReq {
    direction: String,
    #[serde(default)]
    aspect: Option<String>,
}

#[derive(Template)]
#[template(path = "index.html", escape = "none")]
struct IndexTemplate {
    width: u16,
    height: u16,
    aspect_str: String,
    disabled_attr: String,
    hint_text: String,
    portrait: bool,
    land_checked: String,
    port_checked: String,
}

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
        .route("/", get(index))
        .route("/info", get(info))
        .route("/status", get(status))
        .route("/upload", post(upload))
        .route("/calibrate/start", post(calibrate_start))
        .route("/calibrate/answer", post(calibrate_answer))
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

async fn index(State(state): State<AppState>) -> impl IntoResponse {
    let busy = !state.busy.try_lock().is_ok();
    let portrait = load_saved_portrait()
        .unwrap_or(matches!(state.probe.display, Some(paperwave::DisplaySpec::El133Uf1{..})));
    let tpl = IndexTemplate {
        width: state.width,
        height: state.height,
        aspect_str: format!("{:.3}", state.aspect),
        disabled_attr: if busy { "disabled".into() } else { String::new() },
        hint_text: if busy {
            "Update in progress, please wait…".into()
        } else {
            String::new()
        },
        portrait,
        land_checked: if !portrait { "checked".into() } else { String::new() },
        port_checked: if portrait { "checked".into() } else { String::new() },
    };
    Html(tpl.render().unwrap())
}

async fn info(State(state): State<AppState>) -> impl IntoResponse {
    let busy = !state.busy.try_lock().is_ok();
    Json(InfoResponse {
        width: state.width,
        height: state.height,
        aspect: state.aspect,
        busy,
    })
}

async fn status(State(state): State<AppState>) -> impl IntoResponse {
    let busy = !state.busy.try_lock().is_ok();
    Json(StatusResponse { busy })
}

async fn calibrate_start(State(state): State<AppState>) -> impl IntoResponse {
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
    State(state): State<AppState>,
    Json(req): Json<CalibrateAnswerReq>,
) -> impl IntoResponse {
    let dir = req.direction.to_lowercase();
    let rotation = match dir.as_str() {
        "up" => paperwave::Rotation::Deg0,
        "right" => paperwave::Rotation::Deg270,
        "down" => paperwave::Rotation::Deg180,
        "left" => paperwave::Rotation::Deg90,
        _ => return (StatusCode::BAD_REQUEST, "invalid direction").into_response(),
    };
    if let Some(aspect) = req.aspect.as_deref() {
        let portrait = aspect.eq_ignore_ascii_case("portrait");
        if let Err(e) = save_portrait(portrait) {
            warn!(error = %e, "Failed saving portrait");
        }
    }
    if let Err(e) = save_rotation(rotation) {
        warn!(error = %e, "Failed saving rotation");
    }
    info!(?rotation, "Calibration saved");
    StatusCode::OK.into_response()
}

async fn upload(State(state): State<AppState>, mut multipart: Multipart) -> impl IntoResponse {
    let guard = match state.busy.try_lock() {
        Ok(g) => g,
        Err(_) => {
            warn!("Upload rejected: display busy");
            return StatusCode::LOCKED.into_response();
        }
    };

    let mut bytes: Option<Vec<u8>> = None;
    let mut saturation: f32 = 0.5;
    let mut lighten: f32 = 0.0;
    let mut rotation_deg: u16 = 0;

    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                if let Some(name) = field.name() {
                    match name {
                        "file" => match field.bytes().await {
                            Ok(b) => {
                                info!(size = b.len(), "Received upload");
                                bytes = Some(b.to_vec());
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed reading upload field");
                                drop(guard);
                                return (StatusCode::BAD_REQUEST, format!("read error: {e}")).into_response();
                            }
                        },
                        "saturation" => {
                            if let Ok(val) = field.text().await {
                                if let Ok(v) = val.parse::<f32>() {
                                    saturation = v.clamp(0.0, 1.0);
                                }
                            }
                        }
                        "lighten" => {
                            if let Ok(val) = field.text().await {
                                if let Ok(v) = val.parse::<f32>() {
                                    lighten = v.clamp(0.0, 1.0);
                                }
                            }
                        }
                        "rotation" => {
                            if let Ok(val) = field.text().await {
                                if let Ok(v) = val.parse::<u16>() {
                                    rotation_deg = match v % 360 {
                                        0 => 0,
                                        90 => 90,
                                        180 => 180,
                                        270 => 270,
                                        _ => 0,
                                    };
                                }
                            }
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

    let img = match image::load_from_memory(&buf) {
        Ok(i) => i,
        Err(e) => {
            warn!(error = %e, "Invalid image upload");
            drop(guard);
            return (StatusCode::BAD_REQUEST, format!("invalid image: {e}")).into_response();
        }
    };
    let img = apply_exif_orientation_bytes(&buf, img);

    let probe = state.probe.clone();
    let rotation_override = load_saved_rotation();
    info!("Starting display update");
    let res = tokio::task::spawn_blocking(move || {
        update_display(&probe, &img, rotation_override, rotation_deg, saturation, lighten)
    })
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
    let total = (base_deg
        + match user_deg % 360 {
            0 => 0,
            90 => 90,
            180 => 180,
            270 => 270,
            _ => 0,
        }) % 360;
    match total {
        0 => paperwave::Rotation::Deg0,
        90 => paperwave::Rotation::Deg90,
        180 => paperwave::Rotation::Deg180,
        270 => paperwave::Rotation::Deg270,
        _ => paperwave::Rotation::Deg0,
    }
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

fn arrow_image(dim: (u16, u16), dir: char) -> DynamicImage {
    use image::{imageops, Rgb, RgbImage};
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
        let _ = std::fs::create_dir_all(&p);
        p.push("state.json");
        return p;
    }
    PathBuf::from("paperwave_state.json")
}

fn portrait_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".config/paperwave");
        let _ = std::fs::create_dir_all(&p);
        p.push("portrait.txt");
        return p;
    }
    PathBuf::from("paperwave_portrait.txt")
}

fn load_saved_portrait() -> Option<bool> {
    let p = portrait_path();
    let s = std::fs::read_to_string(p).ok()?;
    Some(s.trim() == "true")
}

fn save_portrait(portrait: bool) -> std::io::Result<()> {
    std::fs::write(portrait_path(), if portrait { "true" } else { "false" })
}

fn load_saved_rotation() -> Option<paperwave::Rotation> {
    #[derive(Deserialize)]
    struct State {
        rotation_deg: u16,
    }
    let path = config_path();
    let data = std::fs::read(path).ok()?;
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
    struct State {
        rotation_deg: u16,
    }
    let deg = match rot {
        paperwave::Rotation::Deg0 => 0,
        paperwave::Rotation::Deg90 => 90,
        paperwave::Rotation::Deg180 => 180,
        paperwave::Rotation::Deg270 => 270,
    };
    let path = config_path();
    let data = serde_json::to_vec_pretty(&State { rotation_deg: deg }).unwrap();
    std::fs::write(path, data)
}
