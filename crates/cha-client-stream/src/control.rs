//! The control stream's lines: UTF-8 JSON objects, one per line, both ways
//! (`docs/plans/c2-transport.md` §5). Parsing what the streamer says and
//! building what we say, with no I/O.

use cha_client::{Codec, NodeStats, PerfOverlay};
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
    /// The app's performance overlay, if it has one.
    pub overlay: Option<PerfOverlay>,
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
        /// Another session has the floor and this one may take it.
        can_take: bool,
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
    /// The streamer's report, about once a second: the rate it encodes at,
    /// how many frames it has sent in all, and how late it makes them.
    Stats {
        fps: Option<u32>,
        frames_sent: Option<u64>,
        encode_p99_ms: Option<f32>,
        overlay: Option<PerfOverlay>,
    },
    /// The node's CPU, RAM and GPU use, about once a second.
    System(NodeStats),
    /// The answer to an `overlay` request: the level now (`None`: the app has
    /// no overlay), and why it didn't change if it didn't.
    Overlay {
        level: Option<PerfOverlay>,
        error: Option<String>,
    },
    /// `status`, `pointer`, `haptic`, `players`, `trigger`, and
    /// anything newer: not used.
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

/// The streamer's `overlay` field: a level 0 to 4, or `"custom"`; anything
/// else (the field left out) is an app with no overlay.
pub fn parse_overlay(v: Option<&Value>) -> Option<PerfOverlay> {
    match v? {
        Value::String(s) if s == "custom" => Some(PerfOverlay::Custom),
        Value::Number(n) => n
            .as_u64()
            .filter(|l| *l <= 4)
            .map(|l| PerfOverlay::Preset(l as u8)),
        _ => None,
    }
}

fn number(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64).filter(|n| n.is_finite())
}

/// A `system` message as node stats, as `toNodeStats` in the browser's
/// `stats.ts` reads it: `None` without `cpu` and `mem_total`; the GPU's
/// readings only if the node sent them.
fn node_stats(v: &Value) -> Option<NodeStats> {
    let cpu = number(v, "cpu")?;
    let mem_total = number(v, "mem_total")?;
    Some(NodeStats {
        cpu: cpu as f32,
        cores: number(v, "cores").unwrap_or(0.0) as u32,
        load1: number(v, "load1").unwrap_or(0.0) as f32,
        mem_used: number(v, "mem_used").unwrap_or(0.0) as u64,
        mem_total: mem_total as u64,
        gpu: number(v, "gpu").map(|n| n as f32),
        vram_used: number(v, "vram_used").map(|n| n as u64),
        vram_total: number(v, "vram_total").map(|n| n as u64),
        enc: number(v, "enc").map(|n| n as f32),
        dec: number(v, "dec").map(|n| n as f32),
        temp: number(v, "temp").map(|n| n as f32),
        power: number(v, "power").map(|n| n as f32),
        power_limit: number(v, "power_limit").map(|n| n as f32),
        clock: number(v, "clock").map(|n| n as f32),
        streamer_cpu: number(v, "streamer_cpu").unwrap_or(0.0) as f32,
    })
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
                overlay: parse_overlay(s.get("overlay")),
            })
        }
        "floor" => ServerMsg::Floor {
            control: v.get("control").and_then(Value::as_bool).unwrap_or(false),
            viewers: u32_of(&v, "viewers"),
            can_take: v.get("can_take").and_then(Value::as_bool).unwrap_or(false),
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
        "stats" => ServerMsg::Stats {
            fps: v
                .get("fps")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok()),
            frames_sent: v.get("frames_sent").and_then(Value::as_u64),
            encode_p99_ms: v
                .get("composite_to_encoded_ms_p99")
                .and_then(Value::as_f64)
                .map(|n| n as f32),
            overlay: parse_overlay(v.get("overlay")),
        },
        "overlay" => ServerMsg::Overlay {
            level: parse_overlay(v.get("level")),
            error: v.get("error").and_then(Value::as_str).map(str::to_string),
        },
        "system" => ServerMsg::System(node_stats(&v)?),
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

/// Sets the app's performance overlay, 0 (off) to 4 (full) (controller only).
pub fn overlay(level: u8) -> String {
    json!({"t": "overlay", "level": level}).to_string()
}

/// Asks for the keyboard and mouse when another session has them.
pub fn take_control() -> String {
    json!({"t": "take_control"}).to_string()
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
    fn the_overlay_is_a_level_or_custom_or_absent() {
        use cha_client::PerfOverlay::{Custom, Preset};
        let stats = |o: &str| parse(&format!(r#"{{"t":"stats","overlay":{o}}}"#));
        let level = |m| match m {
            Some(ServerMsg::Stats { overlay, .. }) => overlay,
            other => panic!("{other:?}"),
        };
        assert_eq!(level(stats("3")), Some(Preset(3)));
        assert_eq!(level(stats("0")), Some(Preset(0)));
        assert_eq!(level(stats(r#""custom""#)), Some(Custom));
        assert_eq!(level(stats("9")), None);
        assert_eq!(level(stats("null")), None);
        assert_eq!(
            parse(r#"{"t":"overlay","level":2}"#),
            Some(ServerMsg::Overlay {
                level: Some(Preset(2)),
                error: None
            })
        );
        assert_eq!(
            parse(r#"{"t":"overlay","error":"this app has no performance overlay"}"#),
            Some(ServerMsg::Overlay {
                level: None,
                error: Some("this app has no performance overlay".into())
            })
        );
        assert_eq!(overlay(4), r#"{"level":4,"t":"overlay"}"#);
        assert_eq!(take_control(), r#"{"t":"take_control"}"#);
        let hello = r#"{"t":"hello","stream":{"codec":"hevc","width":1,"height":1,"overlay":1}}"#;
        let Some(ServerMsg::Hello(h)) = parse(hello) else {
            panic!()
        };
        assert_eq!(h.overlay, Some(Preset(1)));
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
                viewers: 2,
                can_take: false
            })
        );
        assert_eq!(
            parse(r#"{"t":"floor","control":false,"viewers":3,"can_take":true}"#),
            Some(ServerMsg::Floor {
                control: false,
                viewers: 3,
                can_take: true
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
            parse(r#"{"t":"something_new"}"#),
            Some(ServerMsg::Other("something_new".into()))
        );
        // A `system` line without the basics is dropped, as in the browser.
        assert_eq!(parse(r#"{"t":"system","cpu":3}"#), None);
        assert!(matches!(
            parse(r#"{"t":"cursor","kind":"named","name":"text"}"#),
            Some(ServerMsg::Cursor)
        ));
    }

    #[test]
    fn stats_line() {
        assert_eq!(
            parse(
                r#"{"t":"stats","elapsed_ms":5000,"fps":120,"frames_sent":300,"composite_to_encoded_ms_p99":2.5}"#
            ),
            Some(ServerMsg::Stats {
                fps: Some(120),
                frames_sent: Some(300),
                encode_p99_ms: Some(2.5),
                overlay: None
            })
        );
        assert_eq!(
            parse(r#"{"t":"stats"}"#),
            Some(ServerMsg::Stats {
                fps: None,
                frames_sent: None,
                encode_p99_ms: None,
                overlay: None
            })
        );
    }

    #[test]
    fn system_line_with_and_without_a_gpu() {
        let Some(ServerMsg::System(n)) = parse(
            r#"{"t":"system","cpu":23.5,"cores":16,"load1":2.1,"mem_used":10,"mem_total":20,"streamer_cpu":4.0}"#,
        ) else {
            panic!()
        };
        assert_eq!(
            (n.cpu, n.cores, n.mem_used, n.mem_total),
            (23.5, 16, 10, 20)
        );
        assert_eq!(n.streamer_cpu, 4.0);
        assert!(n.gpu.is_none() && n.vram_total.is_none() && n.temp.is_none());

        let Some(ServerMsg::System(n)) = parse(
            r#"{"t":"system","cpu":50,"cores":8,"load1":1,"mem_used":8589934592,"mem_total":34359738368,"gpu":42,"vram_used":4294967296,"vram_total":12884901888,"enc":20,"dec":0,"temp":61,"power":180.5,"power_limit":320,"clock":1950,"streamer_cpu":40}"#,
        ) else {
            panic!()
        };
        assert_eq!(n.gpu, Some(42.0));
        assert_eq!(n.vram_used, Some(4_294_967_296));
        assert_eq!(
            (n.temp, n.power, n.power_limit, n.clock),
            (Some(61.0), Some(180.5), Some(320.0), Some(1950.0))
        );
        assert_eq!((n.enc, n.dec), (Some(20.0), Some(0.0)));
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
