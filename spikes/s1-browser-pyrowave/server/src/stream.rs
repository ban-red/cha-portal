//! Encoded streams for replay (spike S1c), in the "CHASTRM1" container written by
//! `spikes/s1c-codec-compare/tools/make-streams.py`: magic, u32 header length,
//! JSON header, then per frame u32 size, u32 flags (bit 0 = keyframe) and the
//! payload padded to 4 bytes.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::traffic::Frame;

const MAGIC: &[u8; 8] = b"CHASTRM1";
const EXTENSION: &str = "chastream";

pub struct Stream {
    pub header: Value,
    pub frames: Arc<[Frame]>,
}

/// The streams in one directory, loaded on first use and kept in memory.
pub struct StreamLibrary {
    dir: Option<PathBuf>,
    loaded: Mutex<HashMap<String, Arc<Stream>>>,
}

impl StreamLibrary {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            loaded: Mutex::new(HashMap::new()),
        }
    }

    /// Headers of every stream, each with its `name`.
    pub fn list(&self) -> Result<Vec<Value>> {
        let Some(dir) = &self.dir else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some(EXTENSION) {
                continue;
            }
            let mut file = File::open(&path)?;
            let mut header =
                read_header(&mut file).with_context(|| format!("{}", path.display()))?;
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_owned();
            header["name"] = Value::String(name);
            out.push(header);
        }
        out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        Ok(out)
    }

    pub fn load(&self, name: &str) -> Result<Arc<Stream>> {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("invalid stream name {name:?}");
        }
        if let Some(stream) = self.loaded.lock().expect("stream cache poisoned").get(name) {
            return Ok(Arc::clone(stream));
        }
        let Some(dir) = &self.dir else {
            bail!("server started without --streams")
        };
        let path = dir.join(format!("{name}.{EXTENSION}"));
        let stream =
            Arc::new(read_stream(&path).with_context(|| format!("loading {}", path.display()))?);
        self.loaded
            .lock()
            .expect("stream cache poisoned")
            .insert(name.to_owned(), Arc::clone(&stream));
        Ok(stream)
    }
}

fn read_header(file: &mut File) -> Result<Value> {
    let mut start = [0u8; 12];
    file.read_exact(&mut start)?;
    if &start[..8] != MAGIC {
        bail!("not a CHASTRM1 file");
    }
    let len = u32::from_le_bytes(start[8..12].try_into().expect("4 bytes")) as usize;
    let mut json = vec![0u8; len];
    file.read_exact(&mut json)?;
    Ok(serde_json::from_slice(&json)?)
}

fn read_stream(path: &PathBuf) -> Result<Stream> {
    let mut file = File::open(path)?;
    let header = read_header(&mut file)?;
    let mut rest = Vec::new();
    file.read_to_end(&mut rest)?;

    let mut frames = Vec::new();
    let mut at = 0;
    while at + 8 <= rest.len() {
        let size = u32::from_le_bytes(rest[at..at + 4].try_into().expect("4 bytes")) as usize;
        let flags = u32::from_le_bytes(rest[at + 4..at + 8].try_into().expect("4 bytes"));
        at += 8;
        if at + size > rest.len() {
            bail!("frame {} is truncated", frames.len());
        }
        frames.push(Frame {
            data: rest[at..at + size].to_vec(),
            key: flags & 1 != 0,
        });
        at += size.div_ceil(4) * 4;
    }
    if frames.is_empty() {
        bail!("stream has no frames");
    }
    Ok(Stream {
        header,
        frames: Arc::from(frames),
    })
}
