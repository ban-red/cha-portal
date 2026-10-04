//! Our binding to NVIDIA's hardware encoder: NVENC and the slice of the CUDA
//! driver API it needs, both loaded from the driver at runtime (`dlopen`), so
//! nothing links against NVIDIA libraries at build time.
//!
//! Frames come in zero-copy: the compositor's output buffers are EGL images,
//! registered with CUDA once ([`RegisteredImage`]) and with NVENC once (the
//! encoder caches registrations by surface), so encoding a frame copies
//! nothing.
//!
//! Only Linux is supported; elsewhere every entry point returns an error.

mod cuda;
mod dl;
mod nvenc;
pub mod sys;

use std::fmt;

pub use cuda::{CudaContext, RegisteredImage, Surface};
pub use nvenc::{Codec, Encoder, EncoderConfig, InputFormat, Timings, max_supported_version};

#[derive(Debug, Clone)]
pub struct Error(String);

impl Error {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
