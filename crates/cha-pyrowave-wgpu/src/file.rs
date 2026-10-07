//! The container PyroWave's encode/decode tools use: `"PYROWAVE"`, eight little-endian
//! `i32` params, then per frame a `u32` size and the frame's bytes.
//! Params: width, height, y4m format, chroma (0 = 4:2:0, 1 = 4:4:4), full range,
//! frame rate numerator, frame rate denominator, reserved.

use crate::{Chroma, Error};

#[derive(Debug)]
pub struct PyroWaveFile<'a> {
    pub width: u32,
    pub height: u32,
    pub chroma: Chroma,
    pub full_range: bool,
    pub fps: f64,
    /// One frame's packets each, as views into the original buffer.
    pub frames: Vec<&'a [u8]>,
}

pub fn parse_pyrowave_file(data: &[u8]) -> Result<PyroWaveFile<'_>, Error> {
    if data.len() < 40 || &data[..8] != b"PYROWAVE" {
        return Err(Error::File("missing PYROWAVE magic"));
    }
    let param = |i: usize| i32::from_le_bytes(data[8 + i * 4..12 + i * 4].try_into().unwrap());
    let mut frames = Vec::new();
    let mut offset = 40;
    while offset + 4 <= data.len() {
        let size = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        if offset + size > data.len() {
            break;
        }
        frames.push(&data[offset..offset + size]);
        offset += size;
    }
    let den = if param(6) == 0 { 1 } else { param(6) };
    Ok(PyroWaveFile {
        width: param(0).max(0) as u32,
        height: param(1).max(0) as u32,
        chroma: if param(3) == 1 {
            Chroma::C444
        } else {
            Chroma::C420
        },
        full_range: param(4) != 0,
        fps: f64::from(param(5)) / f64::from(den),
        frames,
    })
}
