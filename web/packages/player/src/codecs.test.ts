import { describe, expect, test } from "bun:test";
import { isTenBit, supportedTenBitCodecs, WEBCODECS } from "./codecs";

describe("the 10-bit codecs", () => {
  test("their WebCodecs strings are the streamer's", () => {
    // crates/cha-streamer/src/codec.rs codec_string().
    expect(WEBCODECS["hevc-10"]).toBe("hev1.2.4.L153.B0");
    expect(WEBCODECS["av1-10"]).toBe("av01.0.13M.10");
    expect(WEBCODECS.hevc).toBe("hev1.1.6.L153.B0");
  });

  test("only they are 10-bit", () => {
    expect(isTenBit("hevc-10")).toBe(true);
    expect(isTenBit("av1-10")).toBe(true);
    for (const c of ["hevc", "h264", "av1", "pyrowave420", "pyrowave444"] as const) expect(isTenBit(c)).toBe(false);
  });

  test("offered where the browser decodes them in hardware", async () => {
    const asked: string[] = [];
    // An M1 Mac: HEVC Main 10 in hardware, no AV1.
    const m1 = async (c: VideoDecoderConfig) => {
      asked.push(`${c.codec} ${c.hardwareAcceleration}`);
      return { supported: c.codec.startsWith("hev1") };
    };
    expect(await supportedTenBitCodecs(m1)).toEqual(["hevc-10"]);
    expect(asked).toEqual(["hev1.2.4.L153.B0 prefer-hardware", "av01.0.13M.10 prefer-hardware"]);
    expect(await supportedTenBitCodecs(async () => ({ supported: true }))).toEqual(["hevc-10", "av1-10"]);
  });

  test("a browser without WebCodecs, or one that throws, offers none", async () => {
    expect(await supportedTenBitCodecs(null)).toEqual([]);
    expect(
      await supportedTenBitCodecs(async () => {
        throw new TypeError("unknown codec");
      }),
    ).toEqual([]);
  });
});
