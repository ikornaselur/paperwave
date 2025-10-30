use axum::extract::{Multipart, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::Json;
use image::DynamicImage;
use paperwave::displays::common::apply_exif_orientation_bytes;
use serde::Deserialize;
use tracing::{error, info, warn};

use super::state::{AppState, InfoResponse, StatusResponse};
use super::templates::IndexTemplate;
use askama::Template;
use super::util::{
    arrow_image, combine_rotation, create_display_from_probe, load_saved_portrait,
    load_saved_rotation, save_portrait, save_rotation,
};

pub async fn index(State(state): State<AppState>) -> impl IntoResponse {
    let busy = !state.busy.try_lock().is_ok();
    let portrait = load_saved_portrait()
        .unwrap_or(matches!(state.probe.display, Some(paperwave::DisplaySpec::El133Uf1 { .. })));
    let calib_deg: u16 = match super::util::load_saved_rotation() {
        Some(paperwave::Rotation::Deg0) | None => 0,
        Some(paperwave::Rotation::Deg90) => 90,
        Some(paperwave::Rotation::Deg180) => 180,
        Some(paperwave::Rotation::Deg270) => 270,
    };
    let is_spectra6 = matches!(state.probe.display, Some(paperwave::DisplaySpec::El133Uf1 { .. }));
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
        calib_deg,
        is_spectra6,
    };
    Html(tpl.render().unwrap())
}

pub async fn info(State(state): State<AppState>) -> impl IntoResponse {
    let busy = !state.busy.try_lock().is_ok();
    Json(InfoResponse { width: state.width, height: state.height, aspect: state.aspect, busy })
}

pub async fn status(State(state): State<AppState>) -> impl IntoResponse {
    let busy = !state.busy.try_lock().is_ok();
    Json(StatusResponse { busy })
}

pub async fn calibrate_start(State(state): State<AppState>) -> impl IntoResponse {
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

#[derive(Deserialize)]
pub struct CalibrateAnswerReq { pub direction: String, #[serde(default)] pub aspect: Option<String> }

pub async fn calibrate_answer(
    State(_state): State<AppState>,
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
        if let Err(e) = save_portrait(portrait) { warn!(error = %e, "Failed saving portrait"); }
    }
    if let Err(e) = save_rotation(rotation) { warn!(error = %e, "Failed saving rotation"); }
    info!(?rotation, "Calibration saved");
    StatusCode::OK.into_response()
}

pub async fn upload(State(state): State<AppState>, mut multipart: Multipart) -> impl IntoResponse {
    let guard = match state.busy.try_lock() {
        Ok(g) => g,
        Err(_) => { warn!("Upload rejected: display busy"); return StatusCode::LOCKED.into_response(); }
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
                            Ok(b) => { info!(size = b.len(), "Received upload"); bytes = Some(b.to_vec()); }
                            Err(e) => { warn!(error = %e, "Failed reading upload field"); drop(guard); return (StatusCode::BAD_REQUEST, format!("read error: {e}")).into_response(); }
                        },
                        "saturation" => { if let Ok(val) = field.text().await { if let Ok(v) = val.parse::<f32>() { saturation = v.clamp(0.0, 1.0); } } }
                        "lighten" => { if let Ok(val) = field.text().await { if let Ok(v) = val.parse::<f32>() { lighten = v.clamp(0.0, 1.0); } } }
                        "rotation" => { if let Ok(val) = field.text().await { if let Ok(v) = val.parse::<u16>() { rotation_deg = match v % 360 { 0=>0, 90=>90, 180=>180, 270=>270, _=>0 }; } } }
                        _ => {}
                    }
                }
            }
            Ok(None) => break,
            Err(e) => { warn!(error = %e, "Multipart parse error"); drop(guard); return (StatusCode::BAD_REQUEST, format!("multipart parse error: {e}")).into_response(); }
        }
    }

    let Some(buf) = bytes else { warn!("Upload missing file field"); drop(guard); return (StatusCode::BAD_REQUEST, "no file provided").into_response() };

    let img = match image::load_from_memory(&buf) { Ok(i) => i, Err(e) => { warn!(error = %e, "Invalid image upload"); drop(guard); return (StatusCode::BAD_REQUEST, format!("invalid image: {e}")).into_response() } };
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
        Ok(()) => { info!("Display update complete"); StatusCode::OK.into_response() }
        Err(e) => { error!(error = %e, "Display update failed"); (StatusCode::INTERNAL_SERVER_ERROR, e).into_response() }
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
