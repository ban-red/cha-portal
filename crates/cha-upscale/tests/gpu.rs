//! The upscaler on the machine's GPU (Metal on the dev Mac; Mesa's lavapipe works
//! with no GPU at all), against a CPU port of SGSR 1's GLSL. Skips with a message
//! when there is no adapter.

use cha_upscale::{EDGE_SHARPNESS, EDGE_THRESHOLD, Filter, Upscaler, aspect_fit};

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
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
    let features = adapter.features() & wgpu::Features::TIMESTAMP_QUERY;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("cha-upscale test"),
        required_features: features,
        ..Default::default()
    }))
    .ok()?;
    Some(Gpu { device, queue })
}

/// An RGBA8 picture.
#[derive(Clone)]
struct Picture {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Picture {
    fn texel(&self, x: i64, y: i64) -> [f32; 4] {
        let x = x.clamp(0, i64::from(self.width) - 1) as usize;
        let y = y.clamp(0, i64::from(self.height) - 1) as usize;
        let i = (y * self.width as usize + x) * 4;
        let c = |k: usize| f32::from(self.rgba[i + k]) / 255.0;
        [c(0), c(1), c(2), c(3)]
    }

    /// What a linear, clamp-to-edge sampler returns at `uv`.
    fn bilinear(&self, u: f32, v: f32) -> [f32; 3] {
        let x = u * self.width as f32 - 0.5;
        let y = v * self.height as f32 - 0.5;
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let mut out = [0.0; 3];
        for (k, o) in out.iter_mut().enumerate() {
            let a = self.texel(x0, y0)[k] * (1.0 - fx) + self.texel(x0 + 1, y0)[k] * fx;
            let b = self.texel(x0, y0 + 1)[k] * (1.0 - fx) + self.texel(x0 + 1, y0 + 1)[k] * fx;
            *o = a * (1.0 - fy) + b * fy;
        }
        out
    }

    /// `textureGather` of the green channel at `uv`: (i0, j1), (i1, j1), (i1, j0), (i0, j0).
    fn gather_green(&self, u: f32, v: f32) -> [f32; 4] {
        let i0 = (u * self.width as f32 - 0.5).floor() as i64;
        let j0 = (v * self.height as f32 - 0.5).floor() as i64;
        let g = |x, y| self.texel(x, y)[1];
        [g(i0, j0 + 1), g(i0 + 1, j0 + 1), g(i0 + 1, j0), g(i0, j0)]
    }
}

fn fast_lanczos2(x: f32) -> f32 {
    let wa = x - 4.0;
    let wb = x * wa - wa;
    wb * wa * wa
}

fn weight_y(dx: f32, dy: f32, c: f32, spread: f32) -> [f32; 2] {
    let x = (dx * dx + dy * dy) * 0.55 + (c.abs() * spread).clamp(0.0, 1.0);
    let w = fast_lanczos2(x);
    [w, w * c]
}

/// SGSR 1 at `uv`, line by line from sgsr1_shader_mobile.frag (RGBA mode).
fn sgsr(p: &Picture, u: f32, v: f32) -> [f32; 3] {
    let color = p.bilinear(u, v);
    let (w, h) = (p.width as f32, p.height as f32);
    let (tx, ty) = (1.0 / w, 1.0 / h);
    let img = (u * w - 0.5, v * h + 0.5);
    let pix = (img.0.floor(), img.1.floor());
    let mut coord = (pix.0 * tx, pix.1 * ty);
    let pl = (img.0 - pix.0, img.1 - pix.1);
    let mut left = p.gather_green(coord.0, coord.1);
    let g = color[1];
    let edge_vote = (left[2] - left[1]).abs() + (g - left[1]).abs() + (g - left[2]).abs();
    if edge_vote <= EDGE_THRESHOLD {
        return color;
    }
    coord.0 += tx;
    let mut right = p.gather_green(coord.0 + tx, coord.1);
    let up = p.gather_green(coord.0, coord.1 - ty);
    let down = p.gather_green(coord.0, coord.1 + ty);
    let mut up_down = [up[3], up[2], down[1], down[0]];
    let mean = (left[1] + left[2] + right[0] + right[3]) * 0.25;
    for c in left
        .iter_mut()
        .chain(right.iter_mut())
        .chain(up_down.iter_mut())
    {
        *c -= mean;
    }
    let center = g - mean;
    let sum: f32 = left
        .iter()
        .chain(right.iter())
        .chain(up_down.iter())
        .map(|c| c.abs())
        .sum();
    let spread = 2.181818 / sum;
    let taps = [
        (pl.0, pl.1 + 1.0, up_down[0]),
        (pl.0 - 1.0, pl.1 + 1.0, up_down[1]),
        (pl.0 - 1.0, pl.1 - 2.0, up_down[2]),
        (pl.0, pl.1 - 2.0, up_down[3]),
        (pl.0 + 1.0, pl.1 - 1.0, left[0]),
        (pl.0, pl.1 - 1.0, left[1]),
        (pl.0, pl.1, left[2]),
        (pl.0 + 1.0, pl.1, left[3]),
        (pl.0 - 1.0, pl.1 - 1.0, right[0]),
        (pl.0 - 2.0, pl.1 - 1.0, right[1]),
        (pl.0 - 2.0, pl.1, right[2]),
        (pl.0 - 1.0, pl.1, right[3]),
    ];
    let (mut wsum, mut csum) = (0.0, 0.0);
    for (dx, dy, c) in taps {
        let [w, wc] = weight_y(dx, dy, c, spread);
        wsum += w;
        csum += wc;
    }
    let max_y = left[1].max(left[2]).max(right[0].max(right[3]));
    let min_y = left[1].min(left[2]).min(right[0].min(right[3]));
    let final_y = (EDGE_SHARPNESS * (csum / wsum)).clamp(min_y, max_y);
    let delta = (final_y - center).clamp(-23.0 / 255.0, 23.0 / 255.0);
    color.map(|c| (c + delta).clamp(0.0, 1.0))
}

/// A picture with what an upscaler is for: hard edges (a bar, a diagonal, a
/// fine grid) next to flat colour.
fn test_picture(width: u32, height: u32) -> Picture {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let (r, g, b) = if x > 4 && x < 9 {
                (230, 220, 40)
            } else if x + y / 2 > width / 2 && x < width - 6 {
                (20, 40, 200)
            } else if x >= width - 6 && (x + y) % 2 == 0 {
                (250, 250, 250)
            } else {
                (60, 70, 80)
            };
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    Picture {
        width,
        height,
        rgba,
    }
}

/// Upscales `picture` into a `target`-sized RGBA8 texture with `filter` and reads it back.
fn run(gpu: &Gpu, picture: &Picture, target: (u32, u32), filter: Filter) -> (Vec<u8>, Upscaler) {
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut up = Upscaler::new(&gpu.device, &gpu.queue, format, format);
    up.source((picture.width, picture.height));
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: up.source_texture().unwrap(),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &picture.rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(picture.width * 4),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: picture.width,
            height: picture.height,
            depth_or_array_layers: 1,
        },
    );
    let out = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("out"),
        size: wgpu::Extent3d {
            width: target.0,
            height: target.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let row = (target.0 * 4).next_multiple_of(256);
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(row * target.1),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    up.draw(
        &mut encoder,
        &out.create_view(&Default::default()),
        target,
        filter,
    )
    .expect("a source");
    encoder.copy_texture_to_buffer(
        out.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: None,
            },
        },
        out.size(),
    );
    gpu.queue.submit([encoder.finish()]);
    up.after_submit();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.unwrap());
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    let data = readback.slice(..).get_mapped_range();
    let mut pixels = Vec::with_capacity((target.0 * target.1 * 4) as usize);
    for y in 0..target.1 {
        let start = (y * row) as usize;
        pixels.extend_from_slice(&data[start..start + (target.0 * 4) as usize]);
    }
    drop(data);
    (pixels, up)
}

/// The largest per-channel difference between the GPU's picture and `expect`
/// at every pixel of the drawn rectangle, in 8-bit steps; and black around it.
fn compare(
    pixels: &[u8],
    picture: &Picture,
    target: (u32, u32),
    expect: impl Fn(&Picture, f32, f32) -> [f32; 3],
) -> u8 {
    let rect = aspect_fit((picture.width, picture.height), target);
    let mut worst = 0u8;
    for y in 0..target.1 {
        for x in 0..target.0 {
            let i = ((y * target.0 + x) * 4) as usize;
            let got = &pixels[i..i + 3];
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let inside =
                px > rect.x && px < rect.x + rect.width && py > rect.y && py < rect.y + rect.height;
            if !inside {
                assert_eq!(got, [0, 0, 0], "a bar at ({x}, {y}) is black");
                continue;
            }
            let u = (px - rect.x) / rect.width;
            let v = (py - rect.y) / rect.height;
            let want = expect(picture, u, v);
            for k in 0..3 {
                let w = (want[k] * 255.0).round() as i32;
                worst = worst.max((i32::from(got[k]) - w).unsigned_abs() as u8);
            }
        }
    }
    worst
}

#[test]
fn bilinear_matches_a_cpu_bilinear_stretch_into_the_letterbox() {
    let Some(gpu) = gpu() else { return };
    let picture = test_picture(32, 18);
    let target = (77, 50); // not a whole multiple, with bars top and bottom
    let (pixels, _) = run(&gpu, &picture, target, Filter::Bilinear);
    let worst = compare(&pixels, &picture, target, |p, u, v| p.bilinear(u, v));
    // GPUs filter with a few bits of subtexel precision (8 on most).
    assert!(worst <= 2, "bilinear off by {worst}/255");
}

#[test]
fn sgsr_matches_the_upstream_algorithm() {
    let Some(gpu) = gpu() else { return };
    let picture = test_picture(32, 18);
    let target = (64, 40);
    let (pixels, _) = run(&gpu, &picture, target, Filter::Sgsr);
    let worst = compare(&pixels, &picture, target, sgsr);
    let vs_bilinear = compare(&pixels, &picture, target, |p, u, v| p.bilinear(u, v));
    assert!(worst <= 3, "SGSR off by {worst}/255");
    // And it did something: the edges differ from a plain stretch.
    assert!(vs_bilinear >= 8, "SGSR is {vs_bilinear}/255 from bilinear");
}

#[test]
fn sgsr_sharpens_edges_and_leaves_flat_colour_alone() {
    let Some(gpu) = gpu() else { return };
    let picture = test_picture(32, 18);
    let target = (96, 54);
    let (soft, _) = run(&gpu, &picture, target, Filter::Bilinear);
    let (sharp, _) = run(&gpu, &picture, target, Filter::Sgsr);
    // The yellow bar's left edge, half way down: steeper with SGSR.
    let row = 27usize;
    let step = |px: &[u8]| {
        (0..target.0 as usize - 1)
            .map(|x| {
                let i = (row * target.0 as usize + x) * 4 + 1;
                (i32::from(px[i + 4]) - i32::from(px[i])).abs()
            })
            .max()
            .unwrap()
    };
    assert!(
        step(&sharp) > step(&soft),
        "SGSR's steepest step {} vs bilinear's {}",
        step(&sharp),
        step(&soft)
    );
    // A flat grey corner is the same either way.
    let i = (6 * target.0 as usize + 2) * 4;
    assert_eq!(&sharp[i..i + 3], &soft[i..i + 3]);
}

#[test]
fn the_pass_is_timed_where_the_device_has_timestamps() {
    let Some(gpu) = gpu() else { return };
    let timestamps = gpu
        .device
        .features()
        .contains(wgpu::Features::TIMESTAMP_QUERY);
    let picture = test_picture(32, 18);
    let (_, up) = run(&gpu, &picture, (64, 36), Filter::Sgsr);
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    let ns = up.take_gpu_ns();
    if timestamps {
        assert!(ns.is_some_and(|ns| ns > 0), "a reading: {ns:?}");
        assert_eq!(up.take_gpu_ns(), None, "taken once");
    } else {
        eprintln!("SKIP: this adapter has no timestamp queries");
        assert_eq!(ns, None);
    }
}

#[test]
fn an_srgb_window_gets_linear_values_to_encode() {
    let Some(gpu) = gpu() else { return };
    // A mid grey through an sRGB target comes back as the same encoded grey.
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut up = Upscaler::new(&gpu.device, &gpu.queue, format, target_format);
    up.source((4, 4));
    gpu.queue.write_texture(
        up.source_texture().unwrap().as_image_copy(),
        &[128u8, 128, 128, 255].repeat(16),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(16),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
    );
    let out = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("out"),
        size: wgpu::Extent3d {
            width: 64,
            height: 64,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: target_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: 256 * 64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    up.draw(
        &mut encoder,
        &out.create_view(&Default::default()),
        (64, 64),
        Filter::Bilinear,
    );
    encoder.copy_texture_to_buffer(
        out.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: None,
            },
        },
        out.size(),
    );
    gpu.queue.submit([encoder.finish()]);
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.unwrap());
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    let data = readback.slice(..).get_mapped_range();
    let centre = (32 * 256 + 32 * 4) as usize;
    for k in 0..3 {
        assert!(data[centre + k].abs_diff(128) <= 1, "{}", data[centre + k]);
    }
}
