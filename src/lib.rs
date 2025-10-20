#[cfg(target_os = "linux")]
pub mod displays;

#[cfg(target_os = "linux")]
pub use displays::uc8159::{InkyUc8159, InkyUc8159Config, Pins, Rotation};

#[cfg(target_os = "linux")]
pub use displays::uc8159::{self as uc8159, Result as Uc8159Result};
