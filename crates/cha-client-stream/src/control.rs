//! The control stream's lines: UTF-8 JSON objects, one per line, both ways
//! (`docs/plans/c2-transport.md` §5). Parsing what the streamer says and
//! building what we say, with no I/O.

use cha_client::Codec;
use serde_json::{Value, json};

/// A line longer than this is not the streamer talking.
const MAX_LINE: usize = 16 << 20;

/// What `hello` says about the stream.
#[derive(Clone, Debug, PartialEq)]
pub struct Hello {
    /// The codec's name as the streamer spells it (`hevc`, `pyrowave420`).
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub max_datagram: u32,
    pub input: bool,
    pub audio: bool,
    pub gamepads: bool,
}

impl Hello {
    /// The codec as the core knows it; `None` for ones it doesn't know.
    pub fn codec(&self) -> Option<Codec> {
        codec_named(&self.codec)
    }
}

pub fn codec_named(name: &str) -> Option<Codec> {
    match name {
        "h264" => Some(Codec::H264),
        "hevc" => Some(Codec::Hevc),
        "av1" => Some(Codec::Av1),
        "pyrowave420" => Some(Codec::PyroWave420),
        "pyrowave444" => Some(Codec::PyroWave444),
        _ => None,
    }
}

/// How the portal and the streamer spell a codec.
pub fn codec_name(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
        Codec::Av1 => "av1",
        Codec::PyroWave420 => "pyrowave420",
        Codec::PyroWave444 => "pyrowave444",
    }
}

/// A line from the streamer.
#[derive(Clone, Debug, PartialEq)]
pub enum ServerMsg {
    Hello(Hello),
    /// Whether this session holds the floor (its input, resize, clipboard and
    /// cursor mode apply), and how many are watching.
    Floor {
        control: bool,
        viewers: u32,
    },
    Pong {
        c: f64,
        s_us: f64,
    },
    /// The size the streamer applied after a `resize`.
    Resized {
        w: u32,
        h: u32,
    },
    Rumble {
        i: u8,
        lo: f32,
        hi: f32,
        ms: u32,
    },
    Led {
        i: u8,
        r: u8,
        g: u8,
        b: u8,
    },
    /// Answer to a codec switch (not asked for yet).
    Codec {
        codec: String,
        error: Option<String>,
    },
    /// Answer to an `fps` request: the rate now, and why not if refused.
    Fps {
        fps: u32,
        error: Option<String>,
    },
    Clipboard,
    Cursor,
    /// `stats`, `system`, `status`, `overlay`, `pointer`, `haptic`,
    /// `players`, `trigger`, and anything newer: not used.
    Other(String),
}

fn u32_of(v: &Value, key: &str) -> u32 {
    v.get(key)
        .and_then(Value::as_u64)
        .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX))
}

fn f32_of(v: &Value, key: &str) -> f32 {
    v.get(key).and_then(Value::as_f64).unwrap_or(0.0) as f32
}

/// One line, or `None` if it isn't a JSON object with a `t`.
pub fn parse(line: &str) -> Option<ServerMsg> {
    let v: Value = serde_json::from_str(line).ok()?;
    let t = v.get("t")?.as_str()?;
    Some(match t {
        "hello" => {
            let s = v.get("stream")?;
            ServerMsg::Hello(Hello {
                codec: s.get("codec")?.as_str()?.to_string(),
                width: u32_of(s, "width"),
                height: u32_of(s, "height"),
                fps: u32_of(s, "fps"),
                max_datagram: u32_of(s, "maxDatagram"),
                input: s.get("input").and_then(Value::as_bool).unwrap_or(false),
                audio: s.get("audio").and_then(Value::as_bool).unwrap_or(false),
                gamepads: s.get("gamepads").and_then(Value::as_bool).unwrap_or(false),
            })
        }
        "floor" => ServerMsg::Floor {
            control: v.get("control").and_then(Value::as_bool).unwrap_or(false),
            viewers: u32_of(&v, "viewers"),
        },
        "pong" => ServerMsg::Pong {
            c: v.get("c")?.as_f64()?,
            s_us: v.get("s_us")?.as_f64()?,
        },
        "resized" => ServerMsg::Resized {
            w: u32_of(&v, "w"),
            h: u32_of(&v, "h"),
        },
        "rumble" => ServerMsg::Rumble {
            i: u8::try_from(u32_of(&v, "i")).ok()?,
            lo: f32_of(&v, "lo"),
            hi: f32_of(&v, "hi"),
            ms: u32_of(&v, "ms"),
        },
        "led" => ServerMsg::Led {
            i: u8::try_from(u32_of(&v, "i")).ok()?,
            r: u32_of(&v, "r").min(255) as u8,
            g: u32_of(&v, "g").min(255) as u8,
            b: u32_of(&v, "b").min(255) as u8,
        },
        "codec" => ServerMsg::Codec {
            codec: v.get("codec")?.as_str()?.to_string(),
            error: v.get("error").and_then(Value::as_str).map(str::to_string),
        },
        "fps" => ServerMsg::Fps {
            fps: u32_of(&v, "fps"),
            error: v.get("error").and_then(Value::as_str).map(str::to_string),
        },
        "clipboard" => ServerMsg::Clipboard,
        "cursor" => ServerMsg::Cursor,
        other => ServerMsg::Other(other.to_string()),
    })
}

/// Splits the control stream's bytes into lines.
#[derive(Debug, Default)]
pub struct LineBuf {
    buf: Vec<u8>,
}

impl LineBuf {
    /// Adds bytes; returns the complete lines (trimmed, none empty). An error
    /// when a line grows past any sane size.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, &'static str> {
        self.buf.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1]);
            let text = text.trim();
            if !text.is_empty() {
                lines.push(text.to_string());
            }
        }
        if self.buf.len() > MAX_LINE {
            return Err("a control line of more than 16 MiB");
        }
        Ok(lines)
    }
}

// ---- what we say ----

pub fn ping(c: f64) -> String {
    json!({"t": "ping", "c": c}).to_string()
}

pub fn keyframe() -> String {
    json!({"t": "keyframe"}).to_string()
}

pub fn rfi(id: u32) -> String {
    json!({"t": "rfi", "id": id}).to_string()
}

/// Whether this client draws the cursor (`true`) or the picture has it.
pub fn cursor(client: bool) -> String {
    json!({"t": "cursor", "client": client}).to_string()
}

/// The frame rates a streamer runs at (`framerate::CHOICES` there).
pub const FPS_CHOICES: [u32; 3] = [60, 90, 120];

/// Asks for a frame rate, one of [`FPS_CHOICES`] (controller only).
pub fn fps(fps: u32) -> String {
    json!({"t": "fps", "fps": fps}).to_string()
}

pub fn resize(w: u32, h: u32) -> String {
    json!({"t": "resize", "w": w, "h": h}).to_string()
}

/// The size the streamer will make of a requested one (clamped to 320x240 ..
/// 3840x2160, then down to a multiple of 8): `compositor::fit_size`.
pub fn fit_size(w: u32, h: u32) -> (u32, u32) {
    let fit = |v: u32, lo: u32, hi: u32| v.clamp(lo, hi) & !7;
    (fit(w, 320, 3840), fit(h, 240, 2160))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fps_asks_and_answers() {
        assert_eq!(fps(120), r#"{"fps":120,"t":"fps"}"#);
        assert_eq!(
            parse(r#"{"t":"fps","fps":90}"#),
            Some(ServerMsg::Fps {
                fps: 90,
                error: None
            })
        );
        assert_eq!(
            parse(r#"{"t":"fps","fps":60,"error":"nope"}"#),
            Some(ServerMsg::Fps {
                fps: 60,
                error: Some("nope".into())
            })
        );
    }

    #[test]
    fn hello_and_floor() {
        let hello = r#"{"t":"hello","stream":{"codec":"hevc","width":2560,"height":1440,"input":true,"audio":true,"gamepads":true,"fps":60,"overlay":null,"transport":"webtransport","maxDatagram":1200}}"#;
        let Some(ServerMsg::Hello(h)) = parse(hello) else {
            panic!()
        };
        assert_eq!(h.codec(), Some(Codec::Hevc));
        assert_eq!(
            (h.width, h.height, h.fps, h.max_datagram),
            (2560, 1440, 60, 1200)
        );
        assert!(h.input && h.audio && h.gamepads);
        assert_eq!(
            parse(r#"{"t":"floor","control":true,"viewers":2}"#),
            Some(ServerMsg::Floor {
                control: true,
                viewers: 2
            })
        );
        let pyro = hello.replace("hevc", "pyrowave420");
        let Some(ServerMsg::Hello(h)) = parse(&pyro) else {
            panic!()
        };
        assert_eq!(h.codec(), Some(Codec::PyroWave420));
        let unknown = hello.replace("hevc", "vp9");
        let Some(ServerMsg::Hello(h)) = parse(&unknown) else {
            panic!()
        };
        assert_eq!(h.codec(), None);
    }

    #[test]
    fn pong_resized_rumble_led() {
        assert_eq!(
            parse(r#"{"t":"pong","c":12345.5,"s_us":98765}"#),
            Some(ServerMsg::Pong {
                c: 12345.5,
                s_us: 98765.0
            })
        );
        assert_eq!(
            parse(r#"{"t":"resized","w":1920,"h":1080,"s_us":5}"#),
            Some(ServerMsg::Resized { w: 1920, h: 1080 })
        );
        assert_eq!(
            parse(r#"{"t":"rumble","i":1,"lo":0.5,"hi":0.25,"ms":200}"#),
            Some(ServerMsg::Rumble {
                i: 1,
                lo: 0.5,
                hi: 0.25,
                ms: 200
            })
        );
        assert_eq!(
            parse(r#"{"t":"led","i":0,"r":255,"g":0,"b":10}"#),
            Some(ServerMsg::Led {
                i: 0,
                r: 255,
                g: 0,
                b: 10
            })
        );
    }

    #[test]
    fn unknown_and_garbled_lines_do_not_matter() {
        assert_eq!(parse("not json"), None);
        assert_eq!(parse(r#"{"x":1}"#), None);
        assert_eq!(parse(r#"[1,2]"#), None);
        assert_eq!(
            parse(r#"{"t":"stats","frames_sent":3}"#),
            Some(ServerMsg::Other("stats".into()))
        );
        assert_eq!(
            parse(r#"{"t":"something_new"}"#),
            Some(ServerMsg::Other("something_new".into()))
        );
        assert!(matches!(
            parse(r#"{"t":"cursor","kind":"named","name":"text"}"#),
            Some(ServerMsg::Cursor)
        ));
    }

    #[test]
    fn lines_split_across_reads() {
        let mut b = LineBuf::default();
        assert_eq!(b.push(b"{\"t\":\"a\"}\n{\"t\"").unwrap(), ["{\"t\":\"a\"}"]);
        assert_eq!(b.push(b":\"b\"}\r\n\n  \n").unwrap(), ["{\"t\":\"b\"}"]);
        assert!(b.push(b"partial").unwrap().is_empty());
    }

    #[test]
    fn what_we_say() {
        let v: Value = serde_json::from_str(&ping(12.5)).unwrap();
        assert_eq!(
            (v["t"].as_str(), v["c"].as_f64()),
            (Some("ping"), Some(12.5))
        );
        assert_eq!(keyframe(), r#"{"t":"keyframe"}"#);
        let v: Value = serde_json::from_str(&rfi(7)).unwrap();
        assert_eq!((v["t"].as_str(), v["id"].as_u64()), (Some("rfi"), Some(7)));
        let v: Value = serde_json::from_str(&cursor(false)).unwrap();
        assert_eq!(v["client"], false);
        let v: Value = serde_json::from_str(&resize(2560, 1440)).unwrap();
        assert_eq!((v["w"].as_u64(), v["h"].as_u64()), (Some(2560), Some(1440)));
    }

    #[test]
    fn the_streamer_rounds_sizes_down_to_8() {
        assert_eq!(fit_size(2560, 1440), (2560, 1440));
        assert_eq!(fit_size(1919, 1079), (1912, 1072));
        assert_eq!(fit_size(100, 100), (320, 240));
        assert_eq!(fit_size(9000, 9000), (3840, 2160));
    }
}
