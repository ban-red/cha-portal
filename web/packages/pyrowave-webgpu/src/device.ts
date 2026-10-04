// WebGPU device and decode pipelines for PyroWave. Mirrors the decode half of
// pyrowave_webgpu_common.cpp (imbcmdth/pyrowave@webgpu, MIT).

import bandDispatchWgsl from "./shaders/band_dispatch.wgsl?raw";
import commonWgsl from "./shaders/common.wgsl?raw";
import dwtCommonWgsl from "./shaders/dwt_common.wgsl?raw";
import idwtWgsl from "./shaders/idwt.wgsl?raw";
import subgroupWgsl from "./shaders/subgroup.wgsl?raw";
import dequantWgsl from "./shaders/wavelet_dequant.wgsl?raw";

/** 1: FP16 storage for the two finest levels (default). 2: FP32 throughout. */
export type Precision = 1 | 2;

export interface PyroWaveDeviceOptions {
  precision?: Precision;
  /** Request `timestamp-query` when the adapter has it. Defaults to true. */
  timestamps?: boolean;
}

type Binding =
  | "storage-read"
  | "storage-read-write"
  | "texture-2d-array"
  | "sampler"
  | "storage-texture-2d-array";

export class PyroWaveDevice {
  private constructor(
    readonly device: GPUDevice,
    readonly precision: Precision,
    readonly timestamps: boolean,
    readonly mirrorRepeatSampler: GPUSampler,
    readonly dequant: GPUComputePipeline,
    readonly idwt: GPUComputePipeline,
    readonly idwtFinal: GPUComputePipeline,
  ) {}

  /** Features PyroWave decode needs from the adapter. */
  static supports(adapter: GPUAdapter): { ok: boolean; reason?: string } {
    if (!adapter.features.has("subgroups")) {
      return { ok: false, reason: "adapter lacks the `subgroups` feature (subgroup-free path not ported yet)" };
    }
    return { ok: true };
  }

  static async create(adapter: GPUAdapter, options: PyroWaveDeviceOptions = {}): Promise<PyroWaveDevice> {
    const support = PyroWaveDevice.supports(adapter);
    if (!support.ok) throw new Error(support.reason);
    const wantTimestamps = (options.timestamps ?? true) && adapter.features.has("timestamp-query");
    const requiredFeatures: GPUFeatureName[] = ["subgroups"];
    if (wantTimestamps) requiredFeatures.push("timestamp-query");
    const device = await adapter.requestDevice({
      label: "pyrowave",
      requiredFeatures,
      // Large frames need bigger storage bindings than the defaults.
      requiredLimits: {
        maxStorageBufferBindingSize: adapter.limits.maxStorageBufferBindingSize,
        maxBufferSize: adapter.limits.maxBufferSize,
      },
    });
    return PyroWaveDevice.fromDevice(device, options.precision ?? 1, wantTimestamps);
  }

  /** Builds the pipelines on a device that already has the `subgroups` feature. */
  static async fromDevice(device: GPUDevice, precision: Precision = 1, timestamps = false): Promise<PyroWaveDevice> {
    if (!device.features.has("subgroups")) throw new Error("device was created without `subgroups`");
    const sampler = device.createSampler({
      label: "pyrowave-mirror-repeat",
      addressModeU: "mirror-repeat",
      addressModeV: "mirror-repeat",
      addressModeW: "mirror-repeat",
      magFilter: "nearest",
      minFilter: "nearest",
      mipmapFilter: "nearest",
    });
    const [dequant, idwt, idwtFinal] = await Promise.all([
      createPipeline(device, precision, "pyrowave-dequant", [commonWgsl, subgroupWgsl, bandDispatchWgsl, dequantWgsl], {
        subgroups: true,
        dwtShared: false,
        entryPoint: "main",
        bindings: [
          [0, "storage-read"],
          [1, "storage-texture-2d-array"],
          [2, "storage-read"],
          [3, "storage-read"],
          [7, "storage-read"],
        ],
      }),
      createPipeline(device, precision, "pyrowave-idwt", [commonWgsl, dwtCommonWgsl, idwtWgsl], {
        subgroups: false,
        dwtShared: true,
        entryPoint: "main",
        bindings: [
          [0, "storage-read"],
          [1, "texture-2d-array"],
          [2, "sampler"],
          [3, "storage-texture-2d-array"],
        ],
      }),
      createPipeline(device, precision, "pyrowave-idwt-final", [commonWgsl, dwtCommonWgsl, idwtWgsl], {
        subgroups: false,
        dwtShared: true,
        entryPoint: "main_final",
        bindings: [
          [0, "storage-read"],
          [1, "texture-2d-array"],
          [2, "sampler"],
          [4, "storage-read-write"],
        ],
      }),
    ]);
    return new PyroWaveDevice(device, precision, timestamps, sampler, dequant, idwt, idwtFinal);
  }
}

interface PipelineSpec {
  subgroups: boolean;
  /** dwt_common.wgsl's shared tile type depends on the precision. */
  dwtShared: boolean;
  entryPoint: string;
  bindings: [number, Binding][];
}

async function createPipeline(
  device: GPUDevice,
  precision: Precision,
  label: string,
  sources: string[],
  spec: PipelineSpec,
): Promise<GPUComputePipeline> {
  let prefix = "";
  if (spec.subgroups) {
    // The shaders run subgroup operations under control flow that is only uniform
    // per cluster of lanes, as the GLSL does. See subgroup.wgsl.
    prefix += "enable subgroups;\ndiagnostic(off, subgroup_uniformity);\n";
  }
  if (spec.dwtShared) {
    prefix +=
      precision === 2
        ? "alias SHARED_VEC2 = vec2<f32>;\n" +
          "fn shared_pack(v: vec2<f32>) -> SHARED_VEC2 { return v; }\n" +
          "fn shared_unpack(v: SHARED_VEC2) -> vec2<f32> { return v; }\n"
        : // pack2x16float does not promise round-to-nearest; round first.
          "alias SHARED_VEC2 = u32;\n" +
          "fn shared_pack(v: vec2<f32>) -> SHARED_VEC2 {\n" +
          "    return pack2x16float(round_to_f16_vec4(vec4<f32>(v, 0.0, 0.0)).xy);\n" +
          "}\n" +
          "fn shared_unpack(v: SHARED_VEC2) -> vec2<f32> { return unpack2x16float(v); }\n";
  }

  const module = device.createShaderModule({ label, code: prefix + sources.join("\n") });
  const info = await module.getCompilationInfo();
  const errors = info.messages.filter((m) => m.type === "error");
  if (errors.length) {
    throw new Error(`${label}: ${errors.map((m) => `${m.lineNum}:${m.linePos} ${m.message}`).join("; ")}`);
  }

  const layout = device.createBindGroupLayout({
    label,
    entries: spec.bindings.map(([binding, type]) => bindGroupLayoutEntry(binding, type)),
  });
  return device.createComputePipelineAsync({
    label,
    layout: device.createPipelineLayout({ label, bindGroupLayouts: [layout] }),
    compute: { module, entryPoint: spec.entryPoint },
  });
}

function bindGroupLayoutEntry(binding: number, type: Binding): GPUBindGroupLayoutEntry {
  const visibility = GPUShaderStage.COMPUTE;
  switch (type) {
    case "storage-read":
      return { binding, visibility, buffer: { type: "read-only-storage" } };
    case "storage-read-write":
      return { binding, visibility, buffer: { type: "storage" } };
    case "texture-2d-array":
      return { binding, visibility, texture: { sampleType: "unfilterable-float", viewDimension: "2d-array" } };
    case "sampler":
      return { binding, visibility, sampler: { type: "non-filtering" } };
    case "storage-texture-2d-array":
      return {
        binding,
        visibility,
        storageTexture: { access: "write-only", format: "r32float", viewDimension: "2d-array" },
      };
  }
}
