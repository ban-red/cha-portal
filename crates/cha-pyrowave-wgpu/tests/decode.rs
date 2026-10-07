//! Headless tests on the machine's GPU (Metal on the dev Mac). Each skips with a
//! message when there is no adapter, no SUBGROUP, or the spike clips are not checked out.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

use cha_pyrowave_wgpu::{
    Chroma, DecodeTimestamps, Decoder, Error, Pipelines, Planes, Precision, YuvRenderer,
    parse_pyrowave_file, parse_sequence_header, split_records,
};

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    timestamps: bool,
}

fn gpu() -> Option<Gpu> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter =
        match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        {
            Ok(a) => a,
            Err(e) => {
                eprintln!("SKIP: no wgpu adapter: {e}");
                return None;
            }
        };
    if !adapter.features().contains(wgpu::Features::SUBGROUP) {
        eprintln!("SKIP: {} lacks Features::SUBGROUP", adapter.get_info().name);
        return None;
    }
    let timestamps = adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY);
    let mut features = wgpu::Features::SUBGROUP;
    if timestamps {
        features |= wgpu::Features::TIMESTAMP_QUERY;
    }
    // Default limits, as the player's device has.
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("pyrowave-test"),
        required_features: features,
        ..Default::default()
    }))
    .expect("device");
    eprintln!(
        "adapter: {} ({:?}), timestamps: {timestamps}",
        adapter.get_info().name,
        adapter.get_info().backend
    );
    Some(Gpu {
        device,
        queue,
        timestamps,
    })
}

fn clips_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spikes/s1b-pyrowave-webgpu/clips")
}

const CLIPS: [&str; 4] = [
    "testsrc2-1440p-420-604k",
    "testsrc2-1440p-444-1229k",
    "mandelbrot-1440p-420-604k",
    "mandelbrot-1440p-444-1229k",
];

fn load(name: &str) -> Option<Vec<u8>> {
    let path = clips_dir().join(format!("{name}.pyrowave"));
    match std::fs::read(&path) {
        Ok(d) => Some(d),
        Err(_) => {
            eprintln!(
                "SKIP: {} not present (run spikes/s1b-pyrowave-webgpu/tools/make-clips.sh)",
                path.display()
            );
            None
        }
    }
}

fn load_ref(name: &str, w: usize, h: usize, chroma: Chroma) -> Option<Planes> {
    let data = std::fs::read(clips_dir().join(format!("{name}.ref.yuv"))).ok()?;
    let (cw, ch) = if chroma == Chroma::C420 {
        (w / 2, h / 2)
    } else {
        (w, h)
    };
    assert_eq!(data.len(), w * h + 2 * cw * ch, "unexpected reference size");
    Some(Planes {
        chroma,
        width: w as u32,
        height: h as u32,
        y: data[..w * h].to_vec(),
        cb: data[w * h..w * h + cw * ch].to_vec(),
        cr: data[w * h + cw * ch..].to_vec(),
    })
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let sse: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| (f64::from(x) - f64::from(y)).powi(2))
        .sum();
    if sse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0 * 255.0 * a.len() as f64 / sse).log10()
    }
}

fn diff(a: &[u8], b: &[u8]) -> (usize, u8) {
    let mismatches = a.iter().zip(b).filter(|(x, y)| x != y).count();
    let max = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| x.abs_diff(y))
        .max()
        .unwrap_or(0);
    (mismatches, max)
}

fn make_decoder(gpu: &Gpu, pipelines: &Pipelines, frame: &[u8]) -> Decoder {
    let h = parse_sequence_header(frame).expect("sequence header");
    Decoder::new(
        &gpu.device,
        &gpu.queue,
        pipelines,
        h.width,
        h.height,
        h.chroma,
    )
    .expect("decoder")
}

fn decode_and_wait(gpu: &Gpu, d: &mut Decoder, frame: &[u8], partial: bool) -> bool {
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    let ok = d
        .encode_frame(&mut enc, frame, partial, None)
        .expect("packets parse");
    gpu.queue.submit([enc.finish()]);
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    ok
}

#[test]
fn header_and_records() {
    let Some(data) = load(CLIPS[0]) else { return };
    let file = parse_pyrowave_file(&data).unwrap();
    assert_eq!(
        (file.width, file.height, file.chroma, file.frames.len()),
        (2560, 1440, Chroma::C420, 60)
    );
    let f = file.frames[0];
    let h = parse_sequence_header(f).unwrap();
    assert_eq!((h.width, h.height, h.chroma), (2560, 1440, Chroma::C420));
    let records = split_records(f);
    assert_eq!(
        records.iter().map(|r| r.len()).sum::<usize>(),
        f.len(),
        "records tile the frame"
    );
    assert_eq!(
        records.len() as u32,
        h.total_blocks + 1,
        "one header + one record per block"
    );
    eprintln!(
        "{}: color {:?}, {} blocks, {} bytes",
        CLIPS[0],
        h.color,
        h.total_blocks,
        f.len()
    );
}

#[test]
fn needs_subgroups() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
        eprintln!("SKIP: no adapter");
        return;
    };
    let (device, _q) = pollster::block_on(adapter.request_device(&Default::default())).unwrap();
    match Pipelines::new(&device, Precision::Fp16) {
        Err(Error::SubgroupsUnsupported) => eprintln!("ok: {}", Error::SubgroupsUnsupported),
        other => panic!("expected SubgroupsUnsupported, got {:?}", other.map(|_| ())),
    }
}

/// Every frame of every clip decodes, frame 0 is compared with the native decoder's
/// output (`*.ref.yuv`), and per-frame decode time is logged.
#[test]
fn clips_decode_match_reference_and_timing() {
    let Some(gpu) = gpu() else { return };
    let pipelines =
        Pipelines::new(&gpu.device, Precision::Fp16).expect("pipelines compile under naga");
    let period = f64::from(gpu.queue.get_timestamp_period());
    for name in CLIPS {
        let Some(data) = load(name) else { continue };
        let file = parse_pyrowave_file(&data).unwrap();
        let mut dec = make_decoder(&gpu, &pipelines, file.frames[0]);

        let qs = gpu.timestamps.then(|| {
            gpu.device.create_query_set(&wgpu::QuerySetDescriptor {
                label: None,
                ty: wgpu::QueryType::Timestamp,
                count: 4,
            })
        });
        let resolve = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 32,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 32,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut wall = Vec::new();
        let mut gpu_ms = Vec::new();
        let mut parse_ms = Vec::new();
        for (i, frame) in file.frames.iter().enumerate() {
            let t0 = Instant::now();
            let mut enc = gpu.device.create_command_encoder(&Default::default());
            let ts = qs.as_ref().map(|q| DecodeTimestamps {
                query_set: q,
                first: 0,
            });
            assert!(
                dec.encode_frame(&mut enc, frame, false, ts).unwrap(),
                "{name} frame {i} not ready"
            );
            parse_ms.push(t0.elapsed().as_secs_f64() * 1e3);
            if let Some(q) = &qs {
                enc.resolve_query_set(q, 0..4, &resolve, 0);
                enc.copy_buffer_to_buffer(&resolve, 0, &readback, 0, 32);
            }
            gpu.queue.submit([enc.finish()]);
            gpu.device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            wall.push(t0.elapsed().as_secs_f64() * 1e3);
            if qs.is_some() {
                readback
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, |r| r.unwrap());
                gpu.device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .unwrap();
                let t: Vec<u64> = bytemuck_u64(&readback.slice(..).get_mapped_range());
                readback.unmap();
                gpu_ms.push([
                    (t[1] - t[0]) as f64 * period / 1e6,
                    (t[3] - t[2]) as f64 * period / 1e6,
                ]);
            }
            if i == 0 {
                let got = dec.planes().read_back(&gpu.device, &gpu.queue);
                if let Some(r) = load_ref(name, 2560, 1440, file.chroma) {
                    let dy = diff(&got.y, &r.y);
                    let dcb = diff(&got.cb, &r.cb);
                    let dcr = diff(&got.cr, &r.cr);
                    eprintln!(
                        "{name} frame 0 vs native decoder: Y {} mismatches (max {}), Cb {} (max {}), Cr {} (max {}); PSNR Y {:.2} dB",
                        dy.0,
                        dy.1,
                        dcb.0,
                        dcb.1,
                        dcr.0,
                        dcr.1,
                        psnr(&got.y, &r.y)
                    );
                    assert!(
                        psnr(&got.y, &r.y) > 50.0
                            && psnr(&got.cb, &r.cb) > 50.0
                            && psnr(&got.cr, &r.cr) > 50.0
                    );
                } else {
                    eprintln!("{name}: no reference yuv, frame 0 not compared");
                }
            }
        }
        let p = |v: &mut Vec<f64>, q: f64| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[((v.len() - 1) as f64 * q) as usize]
        };
        let mut w = wall.clone();
        let (w50, w95) = (p(&mut w, 0.5), p(&mut w, 0.95));
        let mut pm = parse_ms.clone();
        let pm50 = p(&mut pm, 0.5);
        let mut line = format!(
            "{name}: {} frames; host parse+record p50 {pm50:.2} ms; submit->done wall p50 {w50:.2} / p95 {w95:.2} ms (first frame {:.1})",
            file.frames.len(),
            wall[0]
        );
        if !gpu_ms.is_empty() {
            let mut dq: Vec<f64> = gpu_ms.iter().map(|t| t[0]).collect();
            let mut id: Vec<f64> = gpu_ms.iter().map(|t| t[1]).collect();
            let mut tot: Vec<f64> = gpu_ms.iter().map(|t| t[0] + t[1]).collect();
            line += &format!(
                "; GPU dequant p50 {:.2}, idwt p50 {:.2}, total p50 {:.2} / p95 {:.2} ms",
                p(&mut dq, 0.5),
                p(&mut id, 0.5),
                p(&mut tot, 0.5),
                p(&mut tot, 0.95)
            );
        }
        eprintln!("{line}");
    }
}

fn bytemuck_u64(bytes: &[u8]) -> Vec<u64> {
    bytes
        .chunks_exact(8)
        .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// PSNR against frame 0 of the lavfi source the clips were encoded from. Skips cleanly
/// without ffmpeg.
#[test]
fn psnr_against_regenerated_source() {
    let Some(gpu) = gpu() else { return };
    let pipelines = Pipelines::new(&gpu.device, Precision::Fp16).unwrap();
    for name in CLIPS {
        let Some(data) = load(name) else { continue };
        let file = parse_pyrowave_file(&data).unwrap();
        let (src, pix) = name
            .split_once('-')
            .map(|(s, _)| s)
            .zip(Some(file.chroma))
            .unwrap();
        let pix_fmt = if pix == Chroma::C420 {
            "yuv420p"
        } else {
            "yuv444p"
        };
        let out = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("{src}=size=2560x1440:rate=60"),
            ])
            .args(["-frames:v", "1", "-pix_fmt", pix_fmt, "-f", "rawvideo", "-"])
            .stderr(Stdio::null())
            .output();
        let Ok(out) = out.map_err(|e| eprintln!("SKIP: ffmpeg not runnable: {e}")) else {
            return;
        };
        let (w, h) = (2560usize, 1440usize);
        let (cw, ch) = if pix == Chroma::C420 {
            (w / 2, h / 2)
        } else {
            (w, h)
        };
        if out.stdout.len() != w * h + 2 * cw * ch {
            eprintln!("SKIP: ffmpeg gave {} bytes for {src}", out.stdout.len());
            return;
        }
        let mut dec = make_decoder(&gpu, &pipelines, file.frames[0]);
        assert!(decode_and_wait(&gpu, &mut dec, file.frames[0], false));
        let got = dec.planes().read_back(&gpu.device, &gpu.queue);
        let (py, pcb, pcr) = (
            psnr(&got.y, &out.stdout[..w * h]),
            psnr(&got.cb, &out.stdout[w * h..w * h + cw * ch]),
            psnr(&got.cr, &out.stdout[w * h + cw * ch..]),
        );
        eprintln!(
            "{name} frame 0 vs ffmpeg {src} source: PSNR Y {py:.2} dB, Cb {pcb:.2} dB, Cr {pcr:.2} dB"
        );
        assert!(
            py > 30.0 && pcb > 30.0 && pcr > 30.0,
            "decode is too far from its source"
        );
    }
}

/// Dropping block records still decodes (degraded, no error), and a strict decode
/// refuses an incomplete frame.
#[test]
fn partial_frames_decode_degraded() {
    let Some(gpu) = gpu() else { return };
    let pipelines = Pipelines::new(&gpu.device, Precision::Fp16).unwrap();
    for name in [CLIPS[0], CLIPS[1]] {
        let Some(data) = load(name) else { continue };
        let file = parse_pyrowave_file(&data).unwrap();
        let frame = file.frames[3];
        let mut dec = make_decoder(&gpu, &pipelines, frame);

        assert!(decode_and_wait(&gpu, &mut dec, frame, false));
        let full = dec.planes().read_back(&gpu.device, &gpu.queue);

        // Drop every 10th record of the finest level (the highest block indices), ~7%.
        let first_fine = dec.layout().block_meta[0][0][1].block_offset_32x32;
        let mut kept = Vec::new();
        let (mut dropped, mut blocks) = (0, 0);
        for rec in split_records(frame) {
            let w0 = u32::from_le_bytes(rec[..4].try_into().unwrap());
            if w0 >> 31 == 0 {
                let index = u32::from_le_bytes(rec[4..8].try_into().unwrap()) >> 8;
                blocks += 1;
                if index >= first_fine && blocks % 10 == 0 {
                    dropped += 1;
                    continue;
                }
            }
            kept.extend_from_slice(rec);
        }
        assert!(dropped > 0);

        dec.clear();
        dec.push_packet(&kept)
            .expect("a frame missing records still parses");
        assert!(!dec.is_ready(false), "strict decode must wait for the rest");
        assert!(
            dec.is_ready(true),
            "{dropped} of {blocks} dropped should still be decodable"
        );
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        dec.encode(&mut enc, None);
        gpu.queue.submit([enc.finish()]);
        let part = dec.planes().read_back(&gpu.device, &gpu.queue);
        let p = psnr(&part.y, &full.y);
        eprintln!(
            "{name}: dropped {dropped} of {blocks} blocks ({:.1}%): Y PSNR vs full decode {p:.2} dB",
            100.0 * f64::from(dropped) / f64::from(blocks)
        );
        assert!(
            p.is_finite() && p > 25.0 && p < 100.0,
            "degraded but close, got {p}"
        );

        // Losing a coarse-band record is not decodable as a partial frame.
        let coarse: Vec<u8> = split_records(frame)
            .into_iter()
            .take(1)
            .flatten()
            .copied()
            .collect();
        dec.clear();
        dec.push_packet(&coarse).unwrap();
        assert!(!dec.is_ready(true));
    }
}

/// The renderer's output matches a CPU BT.709 limited-range conversion of the decoded
/// planes, and the aspect-fit leaves the bars alone.
#[test]
fn renderer_matches_cpu_conversion() {
    let Some(gpu) = gpu() else { return };
    let pipelines = Pipelines::new(&gpu.device, Precision::Fp16).unwrap();
    for name in [CLIPS[0], CLIPS[1]] {
        let Some(data) = load(name) else { continue };
        let file = parse_pyrowave_file(&data).unwrap();
        let frame = file.frames[0];
        let mut dec = make_decoder(&gpu, &pipelines, frame);
        let mut renderer = YuvRenderer::new(&gpu.device, wgpu::TextureFormat::Rgba8Unorm);
        renderer.set_source(&gpu.device, dec.planes(), None);

        // 1: the target is the picture's size, so every pixel maps 1:1.
        // 2: a square target, so the picture is letterboxed.
        for target in [(2560u32, 1440u32), (1000, 1000)] {
            let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: target.0,
                    height: target.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            let bpr = (target.0 * 4).next_multiple_of(256);
            let out = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: u64::from(bpr * target.1),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut enc = gpu.device.create_command_encoder(&Default::default());
            assert!(dec.encode_frame(&mut enc, frame, false, None).unwrap());
            renderer.set_color(dec.color());
            {
                let mut pass = enc
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color {
                                    r: 1.0,
                                    g: 0.0,
                                    b: 1.0,
                                    a: 1.0,
                                }),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        ..Default::default()
                    })
                    .forget_lifetime();
                renderer.draw(&gpu.queue, &mut pass, target);
            }
            enc.copy_texture_to_buffer(
                tex.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &out,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(bpr),
                        rows_per_image: None,
                    },
                },
                wgpu::Extent3d {
                    width: target.0,
                    height: target.1,
                    depth_or_array_layers: 1,
                },
            );
            gpu.queue.submit([enc.finish()]);
            out.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
            gpu.device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            let px = out.slice(..).get_mapped_range();
            let planes = dec.planes().read_back(&gpu.device, &gpu.queue);
            let vp = renderer.viewport(target).unwrap();

            let cw = if file.chroma == Chroma::C420 {
                1280
            } else {
                2560
            };
            let cpu = |x: usize, y: usize| -> [f32; 3] {
                let (cx, cy) = if file.chroma == Chroma::C420 {
                    (x / 2, y / 2)
                } else {
                    (x, y)
                };
                let yy = (f32::from(planes.y[y * 2560 + x]) - 16.0) / 219.0;
                let cb = (f32::from(planes.cb[cy * cw + cx]) - 128.0) / 224.0;
                let cr = (f32::from(planes.cr[cy * cw + cx]) - 128.0) / 224.0;
                [
                    yy + 1.5748 * cr,
                    yy - 0.1873 * cb - 0.4681 * cr,
                    yy + 1.8556 * cb,
                ]
                .map(|v| v.clamp(0.0, 1.0) * 255.0)
            };
            let (mut worst, mut checked, mut bars_ok, mut off) = (0.0f32, 0u64, true, 0u64);
            for ty in (0..target.1 as usize).step_by(if target.0 == 2560 { 1 } else { 7 }) {
                for tx in (0..target.0 as usize).step_by(if target.0 == 2560 { 1 } else { 5 }) {
                    let o = ty * bpr as usize + tx * 4;
                    let got = [px[o], px[o + 1], px[o + 2]];
                    let inside = tx as f32 >= vp.x
                        && (tx as f32) < vp.x + vp.width
                        && ty as f32 >= vp.y
                        && (ty as f32) < vp.y + vp.height;
                    if !inside {
                        bars_ok &= got == [255, 0, 255];
                        continue;
                    }
                    // The fragment at (tx, ty) samples the picture pixel under its centre.
                    let ux = ((tx as f32 + 0.5 - vp.x) / vp.width * 2560.0) as usize;
                    let uy = ((ty as f32 + 0.5 - vp.y) / vp.height * 1440.0) as usize;
                    let want = cpu(ux.min(2559), uy.min(1439));
                    let err = (0..3)
                        .map(|k| (f32::from(got[k]) - want[k]).abs())
                        .fold(0.0, f32::max);
                    worst = worst.max(err);
                    checked += 1;
                    // Letterboxed, the picture is scaled; the CPU and GPU may floor to neighbours.
                    if err > 3.0 {
                        off += 1;
                    }
                }
            }
            eprintln!(
                "{name} render {target:?}: viewport {vp:?}; {checked} pixels vs CPU BT.709 limited, worst channel error {worst:.2}/255 ({off} over 3); bars untouched: {bars_ok}"
            );
            assert!(bars_ok, "pixels outside the viewport were touched");
            if target.0 == 2560 {
                assert!(worst <= 2.0, "GPU conversion differs from CPU by {worst}");
            } else {
                assert!(
                    off * 100 <= checked,
                    "{off} of {checked} letterboxed pixels are off"
                );
            }
            drop(px);
            out.unmap();
        }
    }
}
