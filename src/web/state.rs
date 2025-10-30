use serde::Serialize;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct AppState {
    pub width: u16,
    pub height: u16,
    pub aspect: f32,
    pub busy: Arc<Mutex<()>>, // lock while an update is in progress
    pub probe: Arc<paperwave::ProbeInfo>,
}

#[derive(Serialize)]
pub struct InfoResponse {
    pub width: u16,
    pub height: u16,
    pub aspect: f32,
    pub busy: bool,
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub busy: bool,
}

