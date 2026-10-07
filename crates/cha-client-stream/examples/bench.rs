//! Receive benchmark: a PyroWave-shaped `cha-stream/1` stream over loopback,
//! with the receiver measured. No node needed.
//!
//! ```text
//! cargo run --release -p cha-client-stream --example bench -- [--fps 60] \
//!     [--datagrams 1275] [--payload 1180] [--secs 10] [--spread-ms 0] \
//!     [--player-ms 0]
//! ```
//!
//! The streamer runs as a child process of this binary (so its CPU does not
//! count against the client's) and sends like `cha-streamer/src/wt.rs`: every
//! datagram of a frame handed to QUIC in one burst, `fps` times a second,
//! flags `KEYFRAME|INTRA`, packets of 1-3 slices split with
//! CONTINUES/CONTINUED, no pacing between datagrams (or, with `--spread-ms`,
//! spread over that many ms like a wire at line rate), a pass-through
//! congestion window. The client is the real [`cha_client_stream::connect`], on a
//! 2-worker multi-thread runtime like `cha-player`'s, its video channel drained
//! by a thread like `pyrowave_loop`.
//!
//! Reported: frames delivered per second, the gap between consecutive
//! deliveries (p50/p99/max), the longest stretch the session task went
//! without processing a datagram, and the client process's CPU time.

use std::any::Any;
use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cha_client::Codec;
use cha_client_stream::control::LineBuf;
use cha_client_stream::{Target, connect};
use cha_proto::{DatagramHeader, Flags, HEADER_LEN, Kind};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wtransport::quinn::TransportConfig;
use wtransport::quinn::congestion::{Controller, ControllerFactory};
use wtransport::{Endpoint, Identity, ServerConfig};

#[derive(Clone, Debug)]
struct Window;
impl Controller for Window {
    fn on_congestion_event(&mut self, _: Instant, _: Instant, _: bool, _: u64) {}
    fn on_mtu_update(&mut self, _: u16) {}
    fn window(&self) -> u64 {
        64 << 20
    }
    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(self.clone())
    }
    fn initial_window(&self) -> u64 {
        64 << 20
    }
    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}
#[derive(Debug)]
struct WindowFactory;
impl ControllerFactory for WindowFactory {
    fn build(self: Arc<Self>, _: Instant, _: u16) -> Box<dyn Controller> {
        Box::new(Window)
    }
}

struct Args {
    fps: u32,
    datagrams: u16,
    payload: usize,
    secs: u64,
    player_ms: u64,
    /// Spread a frame's datagrams over this many ms (a wire at line rate).
    spread_ms: u64,
}

fn parse() -> (Args, Option<Vec<String>>) {
    let mut a = Args {
        fps: 60,
        datagrams: 1275,
        payload: 1180,
        secs: 10,
        player_ms: 0,
        spread_ms: 0,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut serve = false;
    let mut i = 0;
    while i < argv.len() {
        let v = argv.get(i + 1);
        match argv[i].as_str() {
            "--serve" => serve = true,
            "--fps" => a.fps = v.unwrap().parse().unwrap(),
            "--datagrams" => a.datagrams = v.unwrap().parse().unwrap(),
            "--payload" => a.payload = v.unwrap().parse().unwrap(),
            "--secs" => a.secs = v.unwrap().parse().unwrap(),
            "--player-ms" => a.player_ms = v.unwrap().parse().unwrap(),
            "--spread-ms" => a.spread_ms = v.unwrap().parse().unwrap(),
            other => panic!("unknown argument {other}"),
        }
        i += if argv[i] == "--serve" { 1 } else { 2 };
    }
    (a, serve.then_some(argv))
}

fn main() {
    let (args, serve) = parse();
    if serve.is_none() {
        tracing_subscriber::fmt()
            .with_env_filter("cha_client_stream=debug")
            .with_target(false)
            .init();
    }
    if serve.is_some() {
        serve_main(&args);
    } else {
        client_main(&args);
    }
}

fn serve_main(args: &Args) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let identity = Identity::self_signed(["localhost", "127.0.0.1"]).unwrap();
        let hash: String = Sha256::digest(identity.certificate_chain().as_slice()[0].der())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let mut transport = TransportConfig::default();
        transport
            .datagram_send_buffer_size(32 << 20)
            .datagram_receive_buffer_size(Some(1 << 20))
            .max_idle_timeout(Some(Duration::from_secs(10).try_into().unwrap()))
            .keep_alive_interval(Some(Duration::from_secs(2)))
            .congestion_controller_factory(Arc::new(WindowFactory));
        let config = ServerConfig::builder()
            .with_bind_address(SocketAddr::from(([127, 0, 0, 1], 0)))
            .with_custom_transport(identity, transport)
            .build();
        let endpoint = Endpoint::server(config).unwrap();
        println!("{} {hash}", endpoint.local_addr().unwrap().port());
        std::io::stdout().flush().unwrap();
        let request = endpoint.accept().await.await.unwrap();
        let conn = request.accept().await.unwrap();
        let epoch = Instant::now();
        let (mut send, mut recv) = conn.accept_bi().await.unwrap();
        let max_dg = args.payload + HEADER_LEN;
        tokio::spawn(async move {
            let mut lines = LineBuf::default();
            let mut buf = vec![0u8; 4096];
            let mut hello = false;
            while let Ok(Some(n)) = recv.read(&mut buf).await {
                for line in lines.push(&buf[..n]).unwrap() {
                    let v: Value = serde_json::from_str(&line).unwrap();
                    let mut out = Vec::new();
                    if !hello {
                        hello = true;
                        out.push(json!({"t":"hello","stream":{"codec":"pyrowave444","width":2560,"height":1440,
                            "input":true,"audio":true,"gamepads":true,"fps":60,"overlay":null,
                            "transport":"webtransport","maxDatagram":max_dg}}));
                        out.push(json!({"t":"floor","control":false,"viewers":1}));
                    }
                    if v["t"] == "report" && std::env::var_os("BENCH_REPORTS").is_some() {
                        eprintln!("{:>8.1}s report r={} d={} l={}", epoch.elapsed().as_secs_f64(), v["r"], v["d"], v["l"]);
                    }
                    if v["t"] == "ping" {
                        out.push(json!({"t":"pong","c":v["c"],"s_us":epoch.elapsed().as_micros() as u64}));
                    }
                    for o in out {
                        send.write_all(format!("{o}\n").as_bytes()).await.unwrap();
                    }
                }
            }
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        let payload = vec![0x5au8; args.payload];
        let period = Duration::from_secs_f64(1.0 / f64::from(args.fps));
        let mut next = tokio::time::Instant::now();
        let mut id = 0u32;
        let mut d = Vec::with_capacity(max_dg);
        loop {
            // The packets: 1-3 slices each.
            let mut index = 0u16;
            let mut slices_left = 0u32;
            let mut part = 0u32;
            let ts = epoch.elapsed().as_micros() as u32;
            while index < args.datagrams {
                if slices_left == 0 {
                    slices_left = 1 + (u32::from(index) * 7 % 3);
                    slices_left = slices_left.min(u32::from(args.datagrams - index));
                    part = 0;
                }
                slices_left -= 1;
                let mut flags = Flags::KEYFRAME | Flags::INTRA;
                if slices_left > 0 {
                    flags |= Flags::CONTINUES;
                }
                if part > 0 {
                    flags |= Flags::CONTINUED;
                }
                part += 1;
                let h = DatagramHeader {
                    kind: Kind::Video,
                    flags: Flags(flags),
                    stream: 0,
                    fec: 0,
                    frame_id: id,
                    frag_index: index,
                    frag_count: args.datagrams,
                    send_ts_us: ts,
                };
                let mut head = [0u8; HEADER_LEN];
                h.encode(&mut head);
                d.clear();
                d.extend_from_slice(&head);
                d.extend_from_slice(&payload);
                if conn.send_datagram(&d).is_err() {
                    return;
                }
                index += 1;
                let chunk = usize::from(args.datagrams).div_ceil(args.spread_ms.max(1) as usize);
                if args.spread_ms > 0 && usize::from(index) % chunk == 0 {
                    let step = Duration::from_millis(1) * (usize::from(index) / chunk) as u32;
                    tokio::time::sleep_until(next + step).await;
                }
            }
            id += 1;
            next += period;
            tokio::time::sleep_until(next).await;
            if conn.quic_connection().close_reason().is_some() {
                return;
            }
        }
    });
}

fn cpu() -> Duration {
    let mut ru = unsafe { std::mem::zeroed::<libc::rusage>() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    let tv = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
    tv(ru.ru_utime) + tv(ru.ru_stime)
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn client_main(args: &Args) {
    let exe = std::env::current_exe().unwrap();
    let mut child = Command::new(exe)
        .args([
            "--serve",
            "--fps",
            &args.fps.to_string(),
            "--datagrams",
            &args.datagrams.to_string(),
            "--payload",
            &args.payload.to_string(),
            "--spread-ms",
            &args.spread_ms.to_string(),
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let (port, hash) = line.trim().split_once(' ').unwrap();
    let target = Target {
        urls: vec![format!("https://127.0.0.1:{port}/media?codec=pyrowave444")],
        cert_hash: hash.to_string(),
        codec: Codec::PyroWave444,
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("transport")
        .build()
        .unwrap();
    let mut session = rt.block_on(connect(&target, 2560, 1440)).unwrap();
    let mut video = std::mem::replace(&mut session.video, tokio::sync::mpsc::channel(1).1);
    let player_ms = args.player_ms;
    let secs = args.secs;
    let consumer = std::thread::spawn(move || {
        let mut stamps = Vec::new();
        let (mut partial, mut numbers) = (0u64, Vec::new());
        let mut start: Option<Instant> = None;
        while let Some(f) = video.blocking_recv() {
            let now = Instant::now();
            let s = *start.get_or_insert(now);
            stamps.push(now);
            partial += u64::from(f.partial);
            numbers.push(f.number);
            if player_ms > 0 {
                std::thread::sleep(Duration::from_millis(player_ms));
            }
            if now.duration_since(s) > Duration::from_secs(secs) {
                break;
            }
        }
        (stamps, partial, numbers)
    });
    // Scheduling lateness: a 1 ms sleeper on the runtime and one on an OS
    // thread. A late runtime sleeper with an on-time thread one is the
    // runtime (or a task hogging a worker); both late is the OS.
    let beats = Arc::new(std::sync::Mutex::new(Vec::<(f64, f64, f64)>::new()));
    let t0 = Instant::now();
    {
        let beats = beats.clone();
        rt.spawn(async move {
            let mut last = Instant::now();
            loop {
                tokio::time::sleep(Duration::from_millis(1)).await;
                let now = Instant::now();
                let late = now.duration_since(last).as_secs_f64() * 1000.0 - 1.0;
                last = now;
                if late > 10.0 {
                    beats
                        .lock()
                        .unwrap()
                        .push((t0.elapsed().as_secs_f64(), late, 0.0));
                }
            }
        });
    }
    {
        let beats = beats.clone();
        std::thread::spawn(move || {
            let mut last = Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(1));
                let now = Instant::now();
                let late = now.duration_since(last).as_secs_f64() * 1000.0 - 1.0;
                last = now;
                if late > 10.0 {
                    beats
                        .lock()
                        .unwrap()
                        .push((t0.elapsed().as_secs_f64(), 0.0, late));
                }
            }
        });
    }
    let (cpu0, wall0) = (cpu(), Instant::now());
    let (stamps, partial, numbers) = consumer.join().unwrap();
    let (cpu1, wall) = (cpu() - cpu0, wall0.elapsed());
    let mut gaps: Vec<f64> = stamps
        .windows(2)
        .map(|w| w[1].duration_since(w[0]).as_secs_f64() * 1000.0)
        .collect();
    gaps.sort_by(f64::total_cmp);
    let span = stamps
        .last()
        .zip(stamps.first())
        .map_or(1.0, |(l, f)| l.duration_since(*f).as_secs_f64());
    let skipped = numbers
        .last()
        .zip(numbers.first())
        .map_or(0, |(l, f)| (l - f + 1).saturating_sub(numbers.len() as u64));
    let rate_mbps =
        f64::from(args.datagrams) * (args.payload + HEADER_LEN) as f64 * 8.0 * f64::from(args.fps)
            / 1e6;
    println!(
        "offered: {} fps x {} datagrams x {} B = {:.0} Mbit/s ({:.0} datagrams/s)",
        args.fps,
        args.datagrams,
        args.payload + HEADER_LEN,
        rate_mbps,
        f64::from(args.fps) * f64::from(args.datagrams)
    );
    println!(
        "delivered: {} frames in {:.1}s = {:.1} fps; frame numbers skipped {skipped}; partial {partial}",
        stamps.len(),
        span,
        stamps.len() as f64 / span
    );
    println!(
        "inter-delivery ms: p50 {:.1}  p90 {:.1}  p99 {:.1}  max {:.1}  (ideal {:.1})",
        pct(&gaps, 0.5),
        pct(&gaps, 0.9),
        pct(&gaps, 0.99),
        gaps.last().copied().unwrap_or(0.0),
        1000.0 / f64::from(args.fps)
    );
    println!(
        "gaps over 25 ms: {}, over 40 ms: {}, over 100 ms: {}",
        gaps.iter().filter(|g| **g > 25.0).count(),
        gaps.iter().filter(|g| **g > 40.0).count(),
        gaps.iter().filter(|g| **g > 100.0).count()
    );
    println!(
        "client cpu: {:.2}s over {:.1}s = {:.0}% of one core",
        cpu1.as_secs_f64(),
        wall.as_secs_f64(),
        cpu1.as_secs_f64() / wall.as_secs_f64() * 100.0
    );
    for (at, rt_late, th_late) in beats.lock().unwrap().iter() {
        println!("  late sleeper at {at:.2}s: runtime +{rt_late:.0} ms, thread +{th_late:.0} ms");
    }
    session.control.stop(false);
    let _ =
        rt.block_on(async { tokio::time::timeout(Duration::from_secs(2), session.ended).await });
    let _ = child.kill();
    let _ = child.wait();
}
