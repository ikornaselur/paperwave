use std::time::Duration;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum InkyError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("GPIO error: {0}")]
    Gpio(#[from] gpio_cdev::errors::Error),

    #[error("Timed out waiting for {0} after {1:?}")]
    Timeout(&'static str, Duration),

    #[error("Invalid buffer size: expected {expected}, got {received}")]
    InvalidBufferSize { expected: usize, received: usize },

    #[error("Unsupported resolution {0}x{1}")]
    UnsupportedResolution(u16, u16),

    #[error("Image error: {0}")]
    Image(#[from] image::ImageError),

    #[error("Invalid image dimensions: expected {expected:?}, got {received:?}")]
    InvalidImageDimensions {
        expected: (u16, u16),
        received: (u32, u32),
    },
}

pub type Result<T> = std::result::Result<T, InkyError>;
