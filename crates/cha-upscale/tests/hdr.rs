//! The HDR functions (`hdr.wgsl`) run on the machine's GPU (Mesa's lavapipe
//! where there is none), against values worked out in float64 from SMPTE ST
//! 2084, BT.2087 and BT.2390 (docs/plans/hdr-and-vrr.md, step 7.1). Skipped,
//! with a note, where wgpu finds no adapter.

/// What each output slot computes, and its expected value.
const CASES: &[(&str, f64)] = &[
    ("nits_to_pq(203.0)", 0.580689), // BT.2408's reference white: 58% PQ
    ("nits_to_pq(1000.0)", 0.751827),
    ("pq_to_nits(0.5)", 92.245709),
    ("pq_to_nits(1.0)", 10000.0),
    ("eetf(100.0, 4000.0, 1000.0)", 100.0), // below the knee: unchanged
    ("eetf(4000.0, 4000.0, 1000.0)", 1000.0), // the source's peak to the display's
    ("eetf(2000.0, 4000.0, 1000.0)", 974.937174),
    ("eetf(500.0, 1000.0, 1000.0)", 500.0), // a display as bright as the source
    ("bt2020_to_bt709(vec3<f32>(0.0, 1.0, 0.0)).r", -0.587641), // outside BT.709: negative
    ("bt2020_to_bt709(vec3<f32>(0.0, 1.0, 0.0)).g", 1.1329),
    ("bt2020_to_bt709(vec3<f32>(0.0, 1.0, 0.0)).b", -0.100579),
    // PQ reference white comes out as 1.0, SDR white, on every channel.
    (
        "hdr10_to_display(vec3<f32>(nits_to_pq(203.0)), 203.0, 1000.0, 1000.0).r",
        1.0,
    ),
    (
        "hdr10_to_display(vec3<f32>(nits_to_pq(203.0)), 203.0, 1000.0, 1000.0).g",
        1.0,
    ),
    (
        "hdr10_to_display(vec3<f32>(nits_to_pq(203.0)), 203.0, 1000.0, 1000.0).b",
        1.0,
    ),
    ("srgb_encode(2.0)", 1.353256),   // extended range keeps going
    ("srgb_encode(-0.5)", -0.735357), // and mirrors for negatives
];

#[test]
fn the_hdr_functions_compute_what_the_standards_say() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter =
        match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        {
            Ok(a) => a,
            Err(e) => {
                eprintln!("SKIP: no wgpu adapter: {e}");
                return;
            }
        };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("hdr test"),
        ..Default::default()
    }))
    .expect("device");

    let body: String = CASES
        .iter()
        .enumerate()
        .map(|(i, (expr, _))| format!("    out[{i}] = {expr};\n"))
        .collect();
    let source = format!(
        "{}\n@group(0) @binding(0) var<storage, read_write> out: array<f32, {}>;\n\
         @compute @workgroup_size(1) fn main() {{\n{body}}}\n",
        cha_upscale::HDR_SHADER,
        CASES.len()
    );
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("hdr test"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("hdr test"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let size = (CASES.len() * 4) as u64;
    let out = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("out"),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("hdr test"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: out.as_entire_binding(),
        }],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&out, 0, &readback, 0, size);
    queue.submit([encoder.finish()]);
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.unwrap());
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    let data = readback.slice(..).get_mapped_range();
    let got: Vec<f32> = data
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();

    let mut wrong = Vec::new();
    for ((expr, want), got) in CASES.iter().zip(&got) {
        // f32 against float64: a few parts in 10⁴, and a floor near zero.
        let (got, want) = (f64::from(*got), *want);
        let tolerance = (want.abs() * 3e-4).max(2e-4);
        if (got - want).abs() > tolerance {
            wrong.push(format!("{expr}: {got} (want {want})"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
