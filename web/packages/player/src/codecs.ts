// The hardware codecs' WebCodecs names, and which 10-bit ones this browser decodes
// (docs/plans/hdr-and-vrr.md, step 6). Kept apart from player.ts so it is tested on its own.

import type { Codec } from "./player";

/** The codecs WebCodecs decodes: NVENC's three, and HEVC Main 10 and AV1 10-bit. */
export type HwCodec = "hevc" | "h264" | "av1" | "hevc-10" | "av1-10";

/** WebCodecs names for what NVENC sends (Annex B, parameter sets in band); the streamer's `/streams` says the same. */
export const WEBCODECS: Record<HwCodec, string> = {
  hevc: "hev1.1.6.L153.B0",
  h264: "avc1.640033",
  av1: "av01.0.13M.08",
  "hevc-10": "hev1.2.4.L153.B0",
  "av1-10": "av01.0.13M.10",
};

/** HEVC Main 10 and AV1 10-bit: WebCodecs only (WebRTC's decoders fall back to 8-bit software ones). */
export function isTenBit(codec: Codec): codec is "hevc-10" | "av1-10" {
  return codec === "hevc-10" || codec === "av1-10";
}

type ConfigCheck = (config: VideoDecoderConfig) => Promise<{ supported?: boolean }>;

/**
 * The 10-bit codecs this browser decodes in hardware with WebCodecs, best first (empty where it
 * can't say). AV1 10-bit needs an M3 or later on a Mac; HEVC Main 10 decodes on Apple silicon and
 * on Windows with the HEVC extension.
 */
export async function supportedTenBitCodecs(
  check: ConfigCheck | null = typeof VideoDecoder === "undefined" ? null : (c) => VideoDecoder.isConfigSupported(c),
): Promise<Codec[]> {
  if (!check) return [];
  const out: Codec[] = [];
  for (const codec of ["hevc-10", "av1-10"] as const) {
    try {
      const r = await check({ codec: WEBCODECS[codec], hardwareAcceleration: "prefer-hardware" });
      if (r.supported) out.push(codec);
    } catch {
      // A browser that throws on an unknown string doesn't have it.
    }
  }
  return out;
}
