//! The three compute pipelines (dequant, inverse wavelet, inverse wavelet writing the
//! output planes) and the sampler they share. Mirrors the decode half of
//! `pyrowave_webgpu_common.cpp` (imbcmdth/pyrowave, `webgpu` branch, MIT), through our
//! TypeScript port.

use crate::Error;

const BAND_DISPATCH: &str = include_str!("shaders/band_dispatch.wgsl");
const COMMON: &str = include_str!("shaders/common.wgsl");
const DWT_COMMON: &str = include_str!("shaders/dwt_common.wgsl");
const IDWT: &str = include_str!("shaders/idwt.wgsl");
const SUBGROUP: &str = include_str!("shaders/subgroup.wgsl");
const DEQUANT: &str = include_str!("shaders/wavelet_dequant.wgsl");

/// How the pyramid's two finest levels are stored. Both are r32float textures; this
/// only controls whether values are rounded through FP16 (matching the reference
/// encoder's default) or kept at FP32.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Precision {
    /// FP16 rounding for the two finest levels and for the shared tile (default).
    #[default]
    Fp16,
    /// FP32 throughout.
    Fp32,
}

#[derive(Clone, Copy)]
enum Binding {
    StorageRead,
    StorageReadWrite,
    Texture2dArray,
    Sampler,
    StorageTexture2dArray,
}

struct Spec {
    subgroups: bool,
    /// `dwt_common.wgsl`'s shared tile type depends on the precision.
    dwt_shared: bool,
    entry_point: &'static str,
    bindings: &'static [(u32, Binding)],
}

/// Compiled decode pipelines for one `wgpu::Device`. Build once per device and share
/// across [`crate::Decoder`]s (a resize makes a new decoder, not new pipelines).
#[derive(Clone)]
pub struct Pipelines {
    pub(crate) precision: Precision,
    pub(crate) sampler: wgpu::Sampler,
    pub(crate) dequant: wgpu::ComputePipeline,
    pub(crate) idwt: wgpu::ComputePipeline,
    pub(crate) idwt_final: wgpu::ComputePipeline,
}

impl Pipelines {
    /// Whether `device` can run the decoder (it was created with `Features::SUBGROUP`).
    pub fn supports(device: &wgpu::Device) -> Result<(), Error> {
        if device.features().contains(wgpu::Features::SUBGROUP) {
            Ok(())
        } else {
            Err(Error::SubgroupsUnsupported)
        }
    }

    /// Compiles the shaders. Blocks until wgpu has validated them.
    pub fn new(device: &wgpu::Device, precision: Precision) -> Result<Self, Error> {
        Self::supports(device)?;
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("pyrowave-mirror-repeat"),
            address_mode_u: wgpu::AddressMode::MirrorRepeat,
            address_mode_v: wgpu::AddressMode::MirrorRepeat,
            address_mode_w: wgpu::AddressMode::MirrorRepeat,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        use Binding::*;
        let dequant = create_pipeline(
            device,
            precision,
            "pyrowave-dequant",
            &[COMMON, SUBGROUP, BAND_DISPATCH, DEQUANT],
            &Spec {
                subgroups: true,
                dwt_shared: false,
                entry_point: "main",
                bindings: &[
                    (0, StorageRead),
                    (1, StorageTexture2dArray),
                    (2, StorageRead),
                    (3, StorageRead),
                    (7, StorageRead),
                ],
            },
        )?;
        let idwt = create_pipeline(
            device,
            precision,
            "pyrowave-idwt",
            &[COMMON, DWT_COMMON, IDWT],
            &Spec {
                subgroups: false,
                dwt_shared: true,
                entry_point: "main",
                bindings: &[
                    (0, StorageRead),
                    (1, Texture2dArray),
                    (2, Sampler),
                    (3, StorageTexture2dArray),
                ],
            },
        )?;
        let idwt_final = create_pipeline(
            device,
            precision,
            "pyrowave-idwt-final",
            &[COMMON, DWT_COMMON, IDWT],
            &Spec {
                subgroups: false,
                dwt_shared: true,
                entry_point: "main_final",
                bindings: &[
                    (0, StorageRead),
                    (1, Texture2dArray),
                    (2, Sampler),
                    (4, StorageReadWrite),
                ],
            },
        )?;
        Ok(Pipelines {
            precision,
            sampler,
            dequant,
            idwt,
            idwt_final,
        })
    }
}

fn create_pipeline(
    device: &wgpu::Device,
    precision: Precision,
    label: &'static str,
    sources: &[&str],
    spec: &Spec,
) -> Result<wgpu::ComputePipeline, Error> {
    let mut prefix = String::new();
    if spec.subgroups {
        // naga 29 does not parse `enable subgroups;` (gfx-rs/wgpu#5555) but accepts the
        // subgroup builtins and operations without it, so the directive the browser
        // needs is left out. The shaders run subgroup operations under control flow
        // that is only uniform per cluster of lanes, as the GLSL does; see subgroup.wgsl.
        prefix += "diagnostic(off, subgroup_uniformity);\n";
    }
    if spec.dwt_shared {
        prefix += match precision {
            Precision::Fp32 => {
                "alias SHARED_VEC2 = vec2<f32>;\n\
                 fn shared_pack(v: vec2<f32>) -> SHARED_VEC2 { return v; }\n\
                 fn shared_unpack(v: SHARED_VEC2) -> vec2<f32> { return v; }\n"
            }
            // pack2x16float does not promise round-to-nearest; round first.
            Precision::Fp16 => {
                "alias SHARED_VEC2 = u32;\n\
                 fn shared_pack(v: vec2<f32>) -> SHARED_VEC2 {\n\
                 \x20   return pack2x16float(round_to_f16_vec4(vec4<f32>(v, 0.0, 0.0)).xy);\n\
                 }\n\
                 fn shared_unpack(v: SHARED_VEC2) -> vec2<f32> { return unpack2x16float(v); }\n"
            }
        };
    }
    let code = format!("{prefix}{}", sources.join("\n"));

    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(code.into()),
    });
    let entries: Vec<_> = spec
        .bindings
        .iter()
        .map(|&(binding, ty)| layout_entry(binding, ty))
        .collect();
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &entries,
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&pl),
        module: &module,
        entry_point: Some(spec.entry_point),
        compilation_options: Default::default(),
        cache: None,
    });
    if let Some(err) = pollster::block_on(scope.pop()) {
        return Err(Error::Pipeline {
            label,
            message: err.to_string(),
        });
    }
    Ok(pipeline)
}

fn layout_entry(binding: u32, ty: Binding) -> wgpu::BindGroupLayoutEntry {
    let ty = match ty {
        Binding::StorageRead => wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        Binding::StorageReadWrite => wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: false },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        Binding::Texture2dArray => wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        Binding::Sampler => wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
        Binding::StorageTexture2dArray => wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: wgpu::TextureFormat::R32Float,
            view_dimension: wgpu::TextureViewDimension::D2Array,
        },
    };
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty,
        count: None,
    }
}
