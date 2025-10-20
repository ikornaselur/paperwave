#[cfg(target_os = "linux")]
pub mod uc8159;

#[cfg(target_os = "linux")]
pub use uc8159::{InkyUc8159, InkyUc8159Config, Pins, Rotation};
