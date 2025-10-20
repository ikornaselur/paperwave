#[cfg(target_os = "linux")]
pub mod detect;

#[cfg(target_os = "linux")]
pub mod uc8159;

#[cfg(target_os = "linux")]
pub mod error;

#[cfg(target_os = "linux")]
pub mod common;

#[cfg(target_os = "linux")]
pub mod el133uf1;

#[cfg(target_os = "linux")]
pub use common::{
    InkyDisplay, Rotation, clamp_aspect_resize, distribute_error, nearest_colour,
    pack_buffer_nibbles, pack_luma_nibbles,
};

#[cfg(target_os = "linux")]
pub use detect::{
    DisplaySpec, EepromInfo, I2cBusReport, I2cProbeStatus, ProbeInfo, probe_system,
    uc8159_resolution_from_probe,
};

#[cfg(target_os = "linux")]
pub use uc8159::{InkyUc8159, InkyUc8159Config, Pins};

#[cfg(target_os = "linux")]
pub use el133uf1::{InkyEl133Uf1, InkyEl133Uf1Config, SpectraPins};

#[cfg(target_os = "linux")]
pub use error::{InkyError, Result};
