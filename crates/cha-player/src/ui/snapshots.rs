//! Offscreen renders of the themed UI, for looking at it without a window.
//!
//! ```text
//! CHA_SNAPSHOT_DIR=/some/dir cargo test -p cha-player --release snapshot -- --ignored --nocapture
//! ```
//!
//! Every built-in theme in every variant gets `<theme>-<variant>-launcher.png`
//! (hosts and apps), `-settings.png` (the same with Settings open), `-status.png` (error, info and busy lines)
//! and the stats panel has its own test, `snapshot_stats_panel`, the toolbar
//! `snapshot_toolbar` and the spec's icons `snapshot_icons`. egui is
//! drawn by `egui-wgpu` as in the app: a non-sRGB `Rgba8Unorm` target cleared
//! with the canvas colour as written, at 1280x800 points and 2x.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cha_client::{App, AppState, BoxFuture, Host, Pairing, Session, StreamConfig, Transport};

use super::*;
use crate::theme::{Appearance, Contrast};

const WIDTH_PT: f32 = 1280.0;
const HEIGHT_PT: f32 = 800.0;
const PPP: f32 = 2.0;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// A transport that only lists fixed hosts.
struct Fake {
    name: &'static str,
    hosts: Vec<Host>,
}

impl Transport for Fake {
    fn name(&self) -> &str {
        self.name
    }
    fn hosts(&self) -> Vec<Host> {
        self.hosts.clone()
    }
    fn add_host(&self, _: &str) -> BoxFuture<'_, anyhow::Result<Host>> {
        Box::pin(async { anyhow::bail!("fake") })
    }
    fn pair(&self, _: &str) -> BoxFuture<'_, anyhow::Result<Pairing>> {
        Box::pin(async { anyhow::bail!("fake") })
    }
    fn apps(&self, _: &str) -> BoxFuture<'_, anyhow::Result<Vec<App>>> {
        Box::pin(async { anyhow::bail!("fake") })
    }
    fn launch(&self, _: &str, _: u32, _: StreamConfig) -> BoxFuture<'_, anyhow::Result<Session>> {
        Box::pin(async { anyhow::bail!("fake") })
    }
}

fn host(id: &str, name: &str, address: &str, paired: bool, running: Option<u32>) -> Host {
    Host {
        id: id.into(),
        name: name.into(),
        address: address.into(),
        paired,
        running_app: running,
    }
}

fn app(id: u32, name: &str, state: AppState) -> App {
    App {
        id,
        name: name.into(),
        hdr: false,
        state,
    }
}

fn transports() -> Vec<Box<dyn Transport>> {
    vec![
        Box::new(Fake {
            name: "Cha Portal",
            hosts: vec![
                host(
                    "portal.home.lan",
                    "portal.home.lan",
                    "portal.home.lan",
                    true,
                    None,
                ),
                host(
                    "portal.work.example",
                    "portal.work.example",
                    "portal.work.example",
                    false,
                    None,
                ),
            ],
        }),
        Box::new(Fake {
            name: "Moonlight",
            hosts: vec![
                host("study", "Study PC", "192.168.1.20:47989", true, Some(1)),
                host("attic", "Attic", "192.168.1.31:47989", false, None),
            ],
        }),
    ]
}

fn launcher(config: Config) -> Launcher {
    let mut launcher = Launcher::new(config);
    launcher.set_portal_transport(Some(0));
    let key = (0, "portal.home.lan".to_string());
    launcher.selected = Some(key.clone());
    launcher.apps.insert(
        key,
        AppsState::Loaded {
            apps: vec![
                app(1, "Steam", AppState::Running),
                app(2, "Firefox", AppState::Starting),
                app(3, "Google Chrome", AppState::Stopped),
                app(4, "XFCE Desktop", AppState::Stopped),
                app(5, "KDE Plasma", AppState::Stopped),
            ],
            listed: Instant::now(),
            refreshing: false,
        },
    );
    launcher
}

/// Offscreen egui renderer on its own Metal device.
struct Offscreen {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: egui_wgpu::Renderer,
}

impl Offscreen {
    fn new() -> Self {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .expect("a Metal adapter");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .expect("a device");
        let renderer =
            egui_wgpu::Renderer::new(&device, FORMAT, egui_wgpu::RendererOptions::default());
        Self {
            device,
            queue,
            renderer,
        }
    }

    /// Run `build` until the layout settles, paint the last pass over
    /// `clear`, and return tightly packed RGBA8 pixels.
    fn render(
        &mut self,
        ctx: &egui::Context,
        clear: egui::Color32,
        mut build: impl FnMut(&mut egui::Ui),
    ) -> (u32, u32, Vec<u8>) {
        let (width, height) = ((WIDTH_PT * PPP) as u32, (HEIGHT_PT * PPP) as u32);
        let mut last = None;
        // egui lays out on the first pass and sizes windows on the second;
        // each pass may add font atlas pieces, so every delta is applied.
        for pass in 0..5 {
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(WIDTH_PT, HEIGHT_PT),
                )),
                // Time moves on so windows finish fading in.
                time: Some(f64::from(pass) * 0.5),
                ..Default::default()
            };
            input
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .native_pixels_per_point = Some(PPP);
            let output = ctx.run_ui(input, |ui| build(ui));
            for (id, delta) in &output.textures_delta.set {
                self.renderer
                    .update_texture(&self.device, &self.queue, *id, delta);
            }
            let ppp = output.pixels_per_point;
            last = Some((
                ctx.tessellate(output.shapes, ppp),
                ppp,
                output.textures_delta.free,
            ));
        }
        let (primitives, ppp, free) = last.unwrap();
        assert_eq!(ppp, PPP, "scale in the theme must stay 1.0 here");

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("snapshot"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [width, height],
            pixels_per_point: ppp,
        };
        let extra = self.renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &primitives,
            &screen,
        );
        assert!(extra.is_empty());
        let [r, g, b, _] = clear.to_normalized_gamma_f32();
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("snapshot"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(r),
                            g: f64::from(g),
                            b: f64::from(b),
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.renderer
                .render(&mut pass.forget_lifetime(), &primitives, &screen);
        }
        let padded = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        for id in free {
            self.renderer.free_texture(&id);
        }
        buffer.map_async(wgpu::MapMode::Read, .., |r| r.expect("map the readback"));
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let mapped = buffer.get_mapped_range(..);
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for row in mapped.chunks(padded as usize).take(height as usize) {
            pixels.extend_from_slice(&row[..(width * 4) as usize]);
        }
        (width, height, pixels)
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend(crc32(&body).to_be_bytes());
}

/// An RGB PNG with stored (uncompressed) deflate blocks: big, but no
/// dependency.
fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) {
    let mut raw = Vec::with_capacity((width * 3 + 1) as usize * height as usize);
    for row in rgba.chunks((width * 4) as usize) {
        raw.push(0);
        for px in row.chunks(4) {
            raw.extend_from_slice(&px[..3]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let blocks = raw.chunks(65_535).collect::<Vec<_>>();
    for (i, block) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        z.extend((block.len() as u16).to_le_bytes());
        z.extend((!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend(((b << 16) | a).to_be_bytes());

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend(width.to_be_bytes());
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    std::fs::write(path, out).expect("write the png");
}

fn out_dir() -> PathBuf {
    let dir = PathBuf::from(
        std::env::var_os("CHA_SNAPSHOT_DIR").expect("set CHA_SNAPSHOT_DIR to a folder"),
    );
    assert!(
        dir.is_absolute(),
        "CHA_SNAPSHOT_DIR must be an absolute path"
    );
    std::fs::create_dir_all(&dir).expect("make CHA_SNAPSHOT_DIR");
    dir
}

#[test]
#[ignore = "renders on the GPU into CHA_SNAPSHOT_DIR"]
fn snapshot_themes() {
    let dir = out_dir();
    let data = std::env::temp_dir().join(format!("cha-player-snap-{}", std::process::id()));
    let mut themes = ThemeController::new(data.clone(), None);
    let ctx = egui::Context::default();
    let mut gpu = Offscreen::new();
    let transports = transports();
    let ids: Vec<String> = themes.themes().iter().map(|t| t.id.clone()).collect();
    let variants = [
        ("dark", Appearance::Dark, Contrast::Standard),
        ("light", Appearance::Light, Contrast::Standard),
        ("dark-more", Appearance::Dark, Contrast::More),
        ("light-more", Appearance::Light, Contrast::More),
    ];
    for id in ids {
        for (name, appearance, contrast) in variants {
            let mut config = Config::default();
            config.theme.theme = id.clone();
            config.theme.appearance = appearance;
            config.theme.contrast = contrast;
            themes.sync(&ctx, &config.theme);
            let canvas = themes.canvas();
            let save = |suffix: &str, (w, h, px): (u32, u32, Vec<u8>)| {
                let path = dir.join(format!("{id}-{name}-{suffix}.png"));
                write_png(&path, w, h, &px);
                println!("{}", path.display());
            };

            for (suffix, settings_open) in [("launcher", false), ("settings", true)] {
                let mut l = launcher(config.clone());
                l.settings_open = settings_open;
                save(
                    suffix,
                    gpu.render(&ctx, canvas, |ui| {
                        l.show(ui, &transports, &themes);
                    }),
                );
            }

            let mut l = launcher(config.clone());
            l.error = Some(
                "Could not quit the app: the node did not answer within 10 seconds (is it online?)"
                    .into(),
            );
            l.info = Some("Added Study PC".into());
            l.busy = Some("Starting Firefox…".into());
            save(
                "status",
                gpu.render(&ctx, canvas, |ui| {
                    l.show(ui, &transports, &themes);
                }),
            );
        }
    }
    std::fs::remove_dir_all(data).ok();
}

/// Eight seconds of a stream that has two things wrong: frames dropped on
/// this Mac, and AWDL. The node is a bit hot.
fn stats_history() -> Vec<StatsSnapshot> {
    (0..8)
        .map(|i| StatsSnapshot {
            width: 2560,
            height: 1440,
            codec: "Hevc".into(),
            transport_tag: "WT",
            present_fps: 57.4,
            decode_fps: 60.0,
            target_fps: Some(60),
            sent_fps: Some(60.0),
            mbps: Some(62.4),
            decode_ms: Some(3.18),
            latency_ms: Some(14.2),
            frame_gap_ms: Some(31.0),
            rtt_ms: Some(2.4),
            lost: Some(3),
            recovered: Some(11),
            dropped: 40 + i * 5,
            decode_errors: 1,
            audio_buffer_ms: Some(31.0),
            audio_underruns: 2,
            audio_dropped_ms: 40,
            reconnects: 1,
            awdl_suspected: i > 3,
            node: Some(cha_client::NodeStats {
                cpu: 34.0,
                cores: 16,
                load1: 2.4,
                mem_used: 9_800_000_000,
                mem_total: 34_359_738_368,
                gpu: Some(92.0),
                vram_used: Some(6_700_000_000),
                vram_total: Some(12_884_901_888),
                enc: Some(31.0),
                dec: Some(0.0),
                temp: Some(87.0),
                power: Some(188.0),
                power_limit: Some(320.0),
                clock: Some(1980.0),
                streamer_cpu: 46.0,
            }),
            ..StatsSnapshot::default()
        })
        .collect()
}

/// A stand-in for video: colour bars over a ramp, so the panel's opacity shows.
fn paint_video(ctx: &egui::Context) {
    let painter = egui::Painter::new(
        ctx.clone(),
        egui::LayerId::background(),
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(WIDTH_PT, HEIGHT_PT)),
    );
    let bars = [
        egui::Color32::from_rgb(200, 200, 200),
        egui::Color32::from_rgb(200, 200, 40),
        egui::Color32::from_rgb(40, 200, 200),
        egui::Color32::from_rgb(40, 200, 40),
        egui::Color32::from_rgb(200, 40, 200),
        egui::Color32::from_rgb(200, 40, 40),
        egui::Color32::from_rgb(40, 40, 200),
        egui::Color32::from_gray(30),
    ];
    let w = WIDTH_PT / bars.len() as f32;
    for (i, c) in bars.iter().enumerate() {
        painter.rect_filled(
            egui::Rect::from_min_size(egui::pos2(i as f32 * w, 0.0), egui::vec2(w, HEIGHT_PT)),
            0.0,
            *c,
        );
    }
}

#[test]
#[ignore = "renders on the GPU into CHA_SNAPSHOT_DIR"]
fn snapshot_stats_panel() {
    use crate::overlay_prefs::{OverlayPrefs, Section};

    let dir = out_dir();
    let data = std::env::temp_dir().join(format!("cha-player-snap-stats-{}", std::process::id()));
    let mut themes = ThemeController::new(data.clone(), None);
    let ctx = egui::Context::default();
    let mut gpu = Offscreen::new();
    let history = stats_history();
    let stats = history.last().unwrap().clone();
    let health = crate::health::assess(&history);
    println!(
        "health: {:?} {:?} {:?}",
        health.grade,
        health.score,
        health.issues.iter().map(|i| i.id).collect::<Vec<_>>()
    );
    assert!(health.issues.len() >= 2, "two issues to draw");

    let cases: [(&str, OverlayPrefs, bool); 8] = [
        ("full", OverlayPrefs::default(), false),
        (
            "full-node-folded",
            OverlayPrefs {
                folded: vec![Section::Node],
                ..OverlayPrefs::default()
            },
            false,
        ),
        (
            "compact",
            OverlayPrefs {
                compact: true,
                ..OverlayPrefs::default()
            },
            false,
        ),
        (
            "collapsed",
            OverlayPrefs {
                collapsed: true,
                ..OverlayPrefs::default()
            },
            false,
        ),
        (
            "hidden",
            OverlayPrefs {
                open: false,
                ..OverlayPrefs::default()
            },
            false,
        ),
        (
            "opacity-40",
            OverlayPrefs {
                opacity: 40,
                ..OverlayPrefs::default()
            },
            false,
        ),
        ("settings", OverlayPrefs::default(), true),
        (
            "bottom-right",
            OverlayPrefs {
                corner: crate::overlay_prefs::Corner::BottomRight,
                folded: vec![Section::Stream, Section::Latency],
                ..OverlayPrefs::default()
            },
            false,
        ),
    ];
    for (theme, appearance) in [
        ("cha-magenta", Appearance::Dark),
        ("cha-jade", Appearance::Dark),
        ("cha-magenta", Appearance::Light),
    ] {
        let mut config = Config::default();
        config.theme.theme = theme.into();
        config.theme.appearance = appearance;
        themes.sync(&ctx, &config.theme);
        let tag = format!(
            "{theme}-{}",
            if appearance == Appearance::Light {
                "light"
            } else {
                "dark"
            }
        );
        for (name, prefs, menu) in &cases {
            let mut panel = StatsPanel::new(prefs.clone());
            if *menu {
                panel.open_menu();
            }
            let image = gpu.render(&ctx, egui::Color32::BLACK, |ui| {
                paint_video(ui.ctx());
                panel.show(ui.ctx(), &stats, &health, 0.0);
            });
            let path = dir.join(format!("stats-{tag}-{name}.png"));
            write_png(&path, image.0, image.1, &image.2);
            println!("{}", path.display());
        }
    }
    std::fs::remove_dir_all(data).ok();
}

/// One toolbar picture: what the stream is, what is open, what else is up.
struct ToolbarCase {
    name: &'static str,
    /// "stream", "sound" or "controllers".
    menu: Option<&'static str>,
    folded: bool,
    /// Seconds already counted down.
    power_off: Option<u64>,
    /// A Moonlight stream: no frame rate or overlay to change, no viewers.
    moonlight: bool,
    /// Another session has the controls.
    viewing: bool,
    muted: bool,
    pads: bool,
    denied: bool,
    reconnecting: bool,
    /// The stats panel under the toolbar, in its top corner.
    panel: bool,
    fullscreen: bool,
}

impl ToolbarCase {
    const fn new(name: &'static str) -> Self {
        Self {
            name,
            menu: None,
            folded: false,
            power_off: None,
            moonlight: false,
            viewing: false,
            muted: false,
            pads: false,
            denied: false,
            reconnecting: false,
            panel: false,
            fullscreen: false,
        }
    }
}

fn toolbar_cases() -> Vec<ToolbarCase> {
    vec![
        ToolbarCase::new("bar"),
        ToolbarCase {
            menu: Some("stream"),
            ..ToolbarCase::new("menu-stream")
        },
        ToolbarCase {
            menu: Some("sound"),
            ..ToolbarCase::new("menu-sound")
        },
        ToolbarCase {
            menu: Some("sound"),
            muted: true,
            ..ToolbarCase::new("menu-sound-muted")
        },
        ToolbarCase {
            menu: Some("controllers"),
            pads: true,
            ..ToolbarCase::new("menu-controllers")
        },
        ToolbarCase {
            menu: Some("controllers"),
            denied: true,
            ..ToolbarCase::new("menu-controllers-denied")
        },
        ToolbarCase {
            folded: true,
            ..ToolbarCase::new("folded")
        },
        ToolbarCase {
            power_off: Some(2),
            ..ToolbarCase::new("power-off")
        },
        ToolbarCase {
            moonlight: true,
            ..ToolbarCase::new("moonlight")
        },
        ToolbarCase {
            moonlight: true,
            menu: Some("stream"),
            ..ToolbarCase::new("moonlight-menu-stream")
        },
        ToolbarCase {
            viewing: true,
            menu: Some("stream"),
            ..ToolbarCase::new("viewing-menu-stream")
        },
        ToolbarCase {
            reconnecting: true,
            ..ToolbarCase::new("reconnecting")
        },
        ToolbarCase {
            panel: true,
            pads: true,
            fullscreen: true,
            ..ToolbarCase::new("with-stats-panel")
        },
    ]
}

#[test]
#[ignore = "renders on the GPU into CHA_SNAPSHOT_DIR"]
fn snapshot_toolbar() {
    use crate::input::pads::{InputAccess, PadInfo};
    use crate::overlay_prefs::OverlayPrefs;

    let dir = out_dir();
    let data = std::env::temp_dir().join(format!("cha-player-snap-bar-{}", std::process::id()));
    let mut themes = ThemeController::new(data.clone(), None);
    let ctx = egui::Context::default();
    let mut gpu = Offscreen::new();
    let history = stats_history();
    let stats = history.last().unwrap().clone();
    let health = crate::health::assess(&history);
    let pads = vec![
        PadInfo {
            slot: 0,
            name: "DualSense Wireless Controller".into(),
        },
        PadInfo {
            slot: 1,
            name: "Steam Controller".into(),
        },
    ];

    for theme in ["cha-magenta", "cha-jade"] {
        let mut config = Config::default();
        config.theme.theme = theme.into();
        config.theme.appearance = Appearance::Dark;
        themes.sync(&ctx, &config.theme);
        for case in toolbar_cases() {
            let mut toolbar = Toolbar::new();
            let mut panel = StatsPanel::new(OverlayPrefs::default());
            if case.folded {
                toolbar.on_capture();
            }
            if let Some(menu) = case.menu {
                toolbar.open_menu_for_test(menu);
            }
            if let Some(counted) = case.power_off {
                toolbar.power_off_for_test(Instant::now() - Duration::from_secs(counted));
            }
            let mut stats = stats.clone();
            let link = if case.moonlight {
                stats.transport_tag = "";
                None
            } else {
                Some(cha_client::TransportStats {
                    tag: "WT",
                    target_fps: Some(120),
                    control: Some(!case.viewing),
                    viewers: Some(2),
                    can_take: case.viewing,
                    overlay: Some(cha_client::PerfOverlay::Preset(2)),
                    ..Default::default()
                })
            };
            let transport = if case.moonlight {
                "Moonlight"
            } else {
                "Cha Portal"
            };
            let no_pads: Vec<PadInfo> = Vec::new();
            let image = gpu.render(&ctx, egui::Color32::BLACK, |ui| {
                paint_video(ui.ctx());
                let view = ToolbarView {
                    title: if case.moonlight {
                        "Desktop"
                    } else {
                        "Google Chrome"
                    },
                    transport,
                    stats: &stats,
                    health: &health,
                    link: link.as_ref(),
                    connected: !case.reconnecting,
                    locked: false,
                    mouse: true,
                    muted: case.muted,
                    volume: 65,
                    fullscreen: case.fullscreen,
                    stats_open: case.panel,
                    opacity: 90,
                    pads: if case.pads { &pads } else { &no_pads },
                    input_access: if case.denied {
                        InputAccess::Denied
                    } else {
                        InputAccess::Granted
                    },
                };
                toolbar.show(ui.ctx(), &view);
                if case.panel {
                    panel.show(ui.ctx(), &stats, &health, toolbar.inset());
                }
                if case.reconnecting {
                    show_reconnecting(ui.ctx(), "Reconnecting…", toolbar.inset());
                }
            });
            let path = dir.join(format!("toolbar-{theme}-{}.png", case.name));
            write_png(&path, image.0, image.1, &image.2);
            println!("{}", path.display());
        }
    }
    std::fs::remove_dir_all(data).ok();
}

/// Every spec icon at 12, 16, 24 and 48 points, ink on the panel colour, in
/// magenta dark. The 48 is there to see the shapes; the others are how they ship.
#[test]
#[ignore = "renders on the GPU into CHA_SNAPSHOT_DIR"]
fn snapshot_icons() {
    use crate::theme::icons::{self, Icon};
    use egui::{Align2, FontId, Rect, Sense, pos2, vec2};

    let dir = out_dir();
    let data = std::env::temp_dir().join(format!("cha-player-snap-icons-{}", std::process::id()));
    let mut themes = ThemeController::new(data.clone(), None);
    let ctx = egui::Context::default();
    let mut gpu = Offscreen::new();
    let mut config = Config::default();
    config.theme.theme = "cha-magenta".into();
    config.theme.appearance = Appearance::Dark;
    themes.sync(&ctx, &config.theme);

    let rows = Icon::ALL.len().div_ceil(2);
    let image = gpu.render(&ctx, themes.canvas(), |ui| {
        let p = ui.palette();
        let painter = ui.painter().clone();
        for (n, icon) in Icon::ALL.iter().enumerate() {
            let (col, row) = (n / rows, n % rows);
            let origin = pos2(20.0 + col as f32 * 620.0, 16.0 + row as f32 * 52.0);
            let cell = Rect::from_min_size(origin, vec2(600.0, 48.0));
            painter.rect_filled(cell, 6.0, p.panel);
            painter.text(
                pos2(cell.left() + 10.0, cell.center().y),
                Align2::LEFT_CENTER,
                icon.id(),
                FontId::monospace(12.0),
                p.ink_3,
            );
            let mut x = cell.left() + 200.0;
            for size in [12.0, 16.0, 24.0, 48.0] {
                let r =
                    Rect::from_min_size(pos2(x, cell.center().y - size / 2.0), vec2(size, size));
                icons::paint(&painter, r, *icon, p.ink);
                x += size + 28.0;
            }
        }
        let _ = ui.allocate_exact_size(vec2(1.0, 1.0), Sense::hover());
    });
    let path = dir.join("icons-cha-magenta-dark.png");
    write_png(&path, image.0, image.1, &image.2);
    println!("{}", path.display());
    std::fs::remove_dir_all(data).ok();
}

/// Settings while following a portal: `<variant>-follow.png`.
#[test]
#[ignore = "renders on the GPU into CHA_SNAPSHOT_DIR"]
fn snapshot_follow() {
    use crate::theme::ThemeLook;
    let dir = out_dir();
    let data = std::env::temp_dir().join(format!("cha-player-snap-follow-{}", std::process::id()));
    let mut themes = ThemeController::new(data.clone(), None);
    let ctx = egui::Context::default();
    let mut gpu = Offscreen::new();
    let transports = transports();
    for (name, own, picked) in [
        ("dark", Appearance::Dark, Appearance::Dark),
        ("light", Appearance::Light, Appearance::Light),
    ] {
        let mut config = Config::default();
        config.theme.appearance = own;
        config.theme.follow("portal.home.lan");
        config.theme.store_portal_look(
            "portal.home.lan",
            ThemeLook {
                theme: "cha-jade".into(),
                appearance: picked,
                contrast: Contrast::Standard,
            },
        );
        themes.sync(&ctx, &config.theme);
        let mut l = launcher(config);
        l.settings_open = true;
        l.busy = Some("Starting Firefox…".into());
        let (w, h, px) = gpu.render(&ctx, themes.canvas(), |ui| {
            l.show(ui, &transports, &themes);
        });
        let path = dir.join(format!("{name}-follow.png"));
        write_png(&path, w, h, &px);
        println!("{}", path.display());
    }
    std::fs::remove_dir_all(data).ok();
}
