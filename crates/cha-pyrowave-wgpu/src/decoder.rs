//! PyroWave decoder on wgpu: packets, dequant, inverse wavelet transform, packed 8-bit
//! planes in a GPU buffer that [`crate::YuvRenderer`] samples directly (no readback).
//!
//! Port of `pyrowave_webgpu_decoder.cpp` (imbcmdth/pyrowave, `webgpu` branch, MIT; see
//! `LICENSE-PYROWAVE`), through our TypeScript port in `web/packages/pyrowave-webgpu`.

use std::num::NonZeroU64;

use crate::layout::{
    BlockLayout, Chroma, DECOMPOSITION_LEVELS, NUM_COMPONENTS, NUM_FREQUENCY_BANDS_PER_LEVEL,
    WAVELET_FP16_LEVELS,
};
use crate::parser::{BitstreamParser, ColorInfo};
use crate::{Error, PacketError, Pipelines, Precision};

const DEQUANT_REGISTER_BYTES: usize = 32;
const IDWT_REGISTER_BYTES: usize = 48;
const MAX_WORKGROUPS_X: u32 = 32768;

/// Where the decoded planes live. Samples are 8 bits, four to a `u32` word (sample
/// `x` of a row is byte `x & 3` of word `x >> 2`); `offsets` and `strides` are in
/// words, with rows padded to a multiple of 32 samples. 4:2:0 chroma planes are
/// `width / 2` by `height / 2`.
#[derive(Clone, Debug)]
pub struct PlaneLayout {
    pub buffer: wgpu::Buffer,
    pub width: u32,
    pub height: u32,
    pub chroma: Chroma,
    pub offsets: [u32; 3],
    pub strides: [u32; 3],
}

/// Optional GPU timestamps: the dequant pass writes `first` and `first + 1`, the
/// inverse transform `first + 2` and `first + 3`. Needs `Features::TIMESTAMP_QUERY`.
#[derive(Clone, Copy)]
pub struct DecodeTimestamps<'a> {
    pub query_set: &'a wgpu::QuerySet,
    pub first: u32,
}

struct Dispatch {
    pipeline: wgpu::ComputePipeline,
    bind_group: Option<wgpu::BindGroup>,
    x: u32,
    y: u32,
    z: u32,
}

struct DequantTable {
    buffer: wgpu::Buffer,
    register_bytes: u64,
    ranges_offset: u64,
    ranges_bytes: u64,
}

fn align_up(v: u64, a: u64) -> u64 {
    v.div_ceil(a) * a
}

fn put_u32(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_f32(buf: &mut [u8], at: usize, v: f32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// Decodes PyroWave frames for one fixed size and chroma. A resize is a new decoder.
///
/// Per frame: [`clear`](Self::clear), [`push_packet`](Self::push_packet) for each packet
/// that arrived, then [`encode`](Self::encode) once [`is_ready`](Self::is_ready).
/// [`encode_frame`](Self::encode_frame) does all of that for a frame in one buffer.
pub struct Decoder {
    layout: BlockLayout,
    parser: BitstreamParser,
    planes: PlaneLayout,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipelines: Pipelines,
    offsets_buffer: wgpu::Buffer,
    payload_buffer: wgpu::Buffer,
    payload_capacity: u64,
    // Kept alive: the views and bind groups reference them.
    _pyramid: wgpu::Texture,
    level_views: Vec<wgpu::TextureView>,
    dequant_tables: Vec<DequantTable>,
    dequant_dispatches: Vec<Dispatch>,
    idwt_dispatches: Vec<Dispatch>,
    _idwt_registers: Option<wgpu::Buffer>,
}

impl Decoder {
    /// Creates a decoder on the caller's device. `device` must have `Features::SUBGROUP`
    /// (checked when [`Pipelines`] was made) and `queue` must be its queue.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipelines: &Pipelines,
        width: u32,
        height: u32,
        chroma: Chroma,
    ) -> Result<Self, Error> {
        Pipelines::supports(device)?;
        let layout = BlockLayout::new(width, height, chroma)?;
        let parser = BitstreamParser::new(layout.clone());

        let pyramid = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pyrowave-pyramid"),
            size: wgpu::Extent3d {
                width: layout.aligned_width / 2,
                height: layout.aligned_height / 2,
                depth_or_array_layers: (NUM_FREQUENCY_BANDS_PER_LEVEL * NUM_COMPONENTS) as u32,
            },
            mip_level_count: DECOMPOSITION_LEVELS as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let level_views = (0..DECOMPOSITION_LEVELS as u32)
            .map(|level| {
                pyramid.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("pyrowave-pyramid-level"),
                    dimension: Some(wgpu::TextureViewDimension::D2Array),
                    base_mip_level: level,
                    mip_level_count: Some(1),
                    base_array_layer: 0,
                    array_layer_count: Some(
                        (NUM_FREQUENCY_BANDS_PER_LEVEL * NUM_COMPONENTS) as u32,
                    ),
                    ..Default::default()
                })
            })
            .collect();

        let offsets_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pyrowave-dequant-offsets"),
            size: u64::from(layout.block_count_32x32.max(1)) * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let payload_capacity = 2 * 1024 * 1024;
        let payload_buffer = create_payload_buffer(device, payload_capacity);

        let mut offsets = [0u32; 3];
        let mut strides = [0u32; 3];
        let mut words = 0u32;
        for plane in 0..NUM_COMPONENTS {
            let stride = layout.aligned_plane_width(plane) / 4;
            offsets[plane] = words;
            strides[plane] = stride;
            words += stride * layout.aligned_plane_height(plane);
        }
        let planes = PlaneLayout {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pyrowave-planes"),
                size: u64::from(words) * 4,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            width,
            height,
            chroma,
            offsets,
            strides,
        };

        let mut decoder = Decoder {
            layout,
            parser,
            planes,
            device: device.clone(),
            queue: queue.clone(),
            pipelines: pipelines.clone(),
            offsets_buffer,
            payload_buffer,
            payload_capacity,
            _pyramid: pyramid,
            level_views,
            dequant_tables: Vec::new(),
            dequant_dispatches: Vec::new(),
            idwt_dispatches: Vec::new(),
            _idwt_registers: None,
        };
        decoder.plan_dequant();
        decoder.rebuild_dequant_groups();
        decoder.plan_idwt();
        Ok(decoder)
    }

    /// The planes the next [`encode`](Self::encode) writes. The buffer is created once
    /// and reused for every frame.
    pub fn planes(&self) -> &PlaneLayout {
        &self.planes
    }

    pub fn layout(&self) -> &BlockLayout {
        &self.layout
    }

    /// Colour signalling from the latest sequence header seen.
    pub fn color(&self) -> Option<ColorInfo> {
        self.parser.color()
    }

    pub fn blocks_received(&self) -> u32 {
        self.parser.blocks_received()
    }

    pub fn blocks_expected(&self) -> u32 {
        self.parser.blocks_expected()
    }

    /// Feeds one packet (any number of concatenated records). A corrupt packet is an
    /// error; records before the bad one stay applied.
    pub fn push_packet(&mut self, data: &[u8]) -> Result<(), PacketError> {
        self.parser.push_packet(data)
    }

    /// Whether a decode should be recorded: every block arrived, or (with
    /// `allow_partial`) the coarsest levels are complete and over 90% of blocks arrived.
    pub fn is_ready(&self, allow_partial: bool) -> bool {
        self.parser.is_ready(allow_partial)
    }

    /// Discards any partially received frame.
    pub fn clear(&mut self) {
        self.parser.clear();
    }

    /// Uploads the parsed frame and records dequant plus inverse transform into `encoder`.
    ///
    /// The uploads go through `queue.write_buffer`, which lands before the next
    /// `submit`, so submit `encoder` before calling this again for another frame, or the
    /// second frame's data replaces the first's. Recording into the caller's encoder
    /// lets the player put its render pass in the same submission, with wgpu placing the
    /// barrier between the compute writes and the fragment reads.
    pub fn encode(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        timestamps: Option<DecodeTimestamps<'_>>,
    ) {
        // The dequant shader can read slightly past the end of the payload.
        let required = (self.parser.payload_words().len() * 4) as u64 + 16;
        if required > self.payload_capacity {
            self.payload_capacity = align_up(required * 2, 4);
            self.payload_buffer = create_payload_buffer(&self.device, self.payload_capacity);
            self.rebuild_dequant_groups();
        }

        self.queue.write_buffer(
            &self.offsets_buffer,
            0,
            bytemuck::cast_slice(&self.parser.dequant_offsets),
        );
        let payload = self.parser.payload_words();
        if !payload.is_empty() {
            self.queue
                .write_buffer(&self.payload_buffer, 0, bytemuck::cast_slice(payload));
        }

        record_pass(
            encoder,
            "pyrowave-dequant",
            &self.dequant_dispatches,
            timestamps,
            0,
        );
        record_pass(
            encoder,
            "pyrowave-idwt",
            &self.idwt_dispatches,
            timestamps,
            2,
        );
        self.parser.mark_frame_decoded();
    }

    /// `clear`, push `data` (one frame's packets), and if ready, [`encode`](Self::encode).
    /// Returns whether a decode was recorded. A corrupt packet is an error, but with
    /// `allow_partial` what parsed before it may still be decoded; the caller decides by
    /// checking the result and calling [`encode`](Self::encode) itself if it wants that.
    pub fn encode_frame(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        data: &[u8],
        allow_partial: bool,
        timestamps: Option<DecodeTimestamps<'_>>,
    ) -> Result<bool, PacketError> {
        self.clear();
        self.push_packet(data)?;
        if !self.is_ready(allow_partial) {
            return Ok(false);
        }
        self.encode(encoder, timestamps);
        Ok(true)
    }

    /// Every band of a level in one dispatch (`band_dispatch.wgsl`); levels stay separate.
    fn plan_dequant(&mut self) {
        let layout = &self.layout;
        let align = u64::from(self.device.limits().min_storage_buffer_offset_alignment);
        let fp16 = self.pipelines.precision == Precision::Fp16;
        for level in 0..DECOMPOSITION_LEVELS {
            let mut registers: Vec<[u8; DEQUANT_REGISTER_BYTES]> = Vec::new();
            let mut ranges: Vec<u32> = Vec::new();
            let mut total_workgroups = 0u32;
            for component in 0..NUM_COMPONENTS {
                if level == 0 && component != 0 && layout.chroma == Chroma::C420 {
                    continue;
                }
                let first_band = if level == DECOMPOSITION_LEVELS - 1 {
                    0
                } else {
                    1
                };
                for band in first_band..4 {
                    let meta = layout.block_meta[component][level][band];
                    let (w, h) = (layout.level_width(level), layout.level_height(level));
                    let mut regs = [0u8; DEQUANT_REGISTER_BYTES];
                    put_u32(&mut regs, 0, w);
                    put_u32(&mut regs, 4, h);
                    put_u32(
                        &mut regs,
                        8,
                        (NUM_FREQUENCY_BANDS_PER_LEVEL * component + band) as u32,
                    );
                    put_u32(&mut regs, 12, meta.block_offset_32x32);
                    put_u32(&mut regs, 16, meta.block_stride_32x32);
                    put_u32(
                        &mut regs,
                        20,
                        u32::from(fp16 && level < WAVELET_FP16_LEVELS),
                    );
                    registers.push(regs);
                    let wx = w.div_ceil(32);
                    let count = wx * h.div_ceil(32);
                    ranges.extend([total_workgroups, wx, count, 0]);
                    total_workgroups += count;
                }
            }

            let register_bytes = (registers.len() * DEQUANT_REGISTER_BYTES) as u64;
            let ranges_offset = align_up(register_bytes, align);
            let ranges_bytes = (ranges.len() * 4) as u64;
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pyrowave-dequant-bands"),
                size: ranges_offset + ranges_bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.queue.write_buffer(&buffer, 0, &registers.concat());
            self.queue
                .write_buffer(&buffer, ranges_offset, bytemuck::cast_slice(&ranges));
            self.dequant_tables.push(DequantTable {
                buffer,
                register_bytes,
                ranges_offset,
                ranges_bytes,
            });

            // The shader numbers workgroups x + y * num_workgroups.x, so large tables spill into y.
            let x = total_workgroups.clamp(1, MAX_WORKGROUPS_X);
            self.dequant_dispatches.push(Dispatch {
                pipeline: self.pipelines.dequant.clone(),
                bind_group: None, // filled by rebuild_dequant_groups()
                x,
                y: total_workgroups.div_ceil(x),
                z: 1,
            });
        }
    }

    /// Dequant bind groups reference the payload buffer, which grows with frame size.
    fn rebuild_dequant_groups(&mut self) {
        let bgl = self.pipelines.dequant.get_bind_group_layout(0);
        for (level, table) in self.dequant_tables.iter().enumerate() {
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pyrowave-dequant"),
                layout: &bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &table.buffer,
                            offset: 0,
                            size: NonZeroU64::new(table.register_bytes),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&self.level_views[level]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.offsets_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self.payload_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &table.buffer,
                            offset: table.ranges_offset,
                            size: NonZeroU64::new(table.ranges_bytes),
                        }),
                    },
                ],
            });
            self.dequant_dispatches[level].bind_group = Some(bind_group);
        }
    }

    /// Per level, components writing the next LL band share one dispatch and those
    /// writing an output plane share another, one component per `workgroup_id.z`.
    fn plan_idwt(&mut self) {
        struct Spec {
            input_level: usize,
            final_output: bool,
            regs: Vec<[u8; IDWT_REGISTER_BYTES]>,
            resolution: [u32; 2],
            offset: u64,
        }
        let layout = &self.layout;
        let is420 = layout.chroma == Chroma::C420;
        let fp16 = self.pipelines.precision == Precision::Fp16;
        let align = u64::from(self.device.limits().min_storage_buffer_offset_alignment);
        let mut specs: Vec<Spec> = Vec::new();

        for input_level in (0..DECOMPOSITION_LEVELS).rev() {
            // Transposed.
            let resolution = [
                layout.level_height(input_level),
                layout.level_width(input_level),
            ];
            let new_spec = |final_output| Spec {
                input_level,
                final_output,
                regs: Vec::new(),
                resolution,
                offset: 0,
            };
            let mut to_pyramid = new_spec(false);
            let mut to_planes = new_spec(true);

            for c in 0..NUM_COMPONENTS {
                if input_level == 0 && is420 && c != 0 {
                    continue;
                }
                let mut regs = [0u8; IDWT_REGISTER_BYTES];
                put_u32(&mut regs, 0, resolution[0]);
                put_u32(&mut regs, 4, resolution[1]);
                put_f32(&mut regs, 8, 1.0 / resolution[0] as f32);
                put_f32(&mut regs, 12, 1.0 / resolution[1] as f32);
                put_u32(&mut regs, 32, (NUM_FREQUENCY_BANDS_PER_LEVEL * c) as u32); // input_layer

                if input_level == 0 || (is420 && c != 0 && input_level == 1) {
                    put_u32(&mut regs, 20, self.planes.offsets[c]);
                    put_u32(&mut regs, 24, self.planes.strides[c]);
                    put_u32(&mut regs, 28, layout.aligned_plane_height(c));
                    to_planes.regs.push(regs);
                } else {
                    put_u32(
                        &mut regs,
                        16,
                        u32::from(fp16 && input_level - 1 < WAVELET_FP16_LEVELS),
                    );
                    put_u32(&mut regs, 36, (NUM_FREQUENCY_BANDS_PER_LEVEL * c) as u32); // output_layer
                    to_pyramid.regs.push(regs);
                }
            }
            if !to_pyramid.regs.is_empty() {
                specs.push(to_pyramid);
            }
            if !to_planes.regs.is_empty() {
                specs.push(to_planes);
            }
        }

        let mut size = 0u64;
        for spec in &mut specs {
            spec.offset = align_up(size, align);
            size = spec.offset + (spec.regs.len() * IDWT_REGISTER_BYTES) as u64;
        }
        let mut table = vec![0u8; align_up(size, 4) as usize];
        for spec in &specs {
            for (i, r) in spec.regs.iter().enumerate() {
                let at = spec.offset as usize + i * IDWT_REGISTER_BYTES;
                table[at..at + IDWT_REGISTER_BYTES].copy_from_slice(r);
            }
        }
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pyrowave-idwt-registers"),
            size: table.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&buffer, 0, &table);

        for spec in &specs {
            let registers = wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &buffer,
                offset: spec.offset,
                size: NonZeroU64::new((spec.regs.len() * IDWT_REGISTER_BYTES) as u64),
            });
            let pipeline = if spec.final_output {
                &self.pipelines.idwt_final
            } else {
                &self.pipelines.idwt
            };
            let input = wgpu::BindingResource::TextureView(&self.level_views[spec.input_level]);
            let sampler = wgpu::BindingResource::Sampler(&self.pipelines.sampler);
            let bind_group = if spec.final_output {
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("pyrowave-idwt-final"),
                    layout: &pipeline.get_bind_group_layout(0),
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: registers,
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: input,
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: sampler,
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: self.planes.buffer.as_entire_binding(),
                        },
                    ],
                })
            } else {
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("pyrowave-idwt"),
                    layout: &pipeline.get_bind_group_layout(0),
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: registers,
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: input,
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: sampler,
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(
                                &self.level_views[spec.input_level - 1],
                            ),
                        },
                    ],
                })
            };
            self.idwt_dispatches.push(Dispatch {
                pipeline: pipeline.clone(),
                bind_group: Some(bind_group),
                x: spec.resolution[0].div_ceil(16),
                y: spec.resolution[1].div_ceil(16),
                z: spec.regs.len() as u32,
            });
        }
        self._idwt_registers = Some(buffer);
    }
}

fn create_payload_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pyrowave-payload"),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn record_pass(
    encoder: &mut wgpu::CommandEncoder,
    label: &str,
    dispatches: &[Dispatch],
    timestamps: Option<DecodeTimestamps<'_>>,
    offset: u32,
) {
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some(label),
        timestamp_writes: timestamps.map(|t| wgpu::ComputePassTimestampWrites {
            query_set: t.query_set,
            beginning_of_pass_write_index: Some(t.first + offset),
            end_of_pass_write_index: Some(t.first + offset + 1),
        }),
    });
    for d in dispatches {
        pass.set_pipeline(&d.pipeline);
        pass.set_bind_group(0, d.bind_group.as_ref(), &[]);
        pass.dispatch_workgroups(d.x, d.y, d.z);
    }
}
