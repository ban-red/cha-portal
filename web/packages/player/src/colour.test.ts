import { describe, expect, test } from "bun:test";
import { codecDepth, colourText, displayLine, displayText, pyroColour, videoFrameColour } from "./colour";

describe("a picture's colour", () => {
  test("today's hardware streams are 8-bit sRGB with BT.709 primaries", () => {
    // What Chrome reports for the NVENC stream's VUI (transfer 13 is IEC 61966-2-1).
    const c = videoFrameColour({ primaries: "bt709", transfer: "iec61966-2-1" }, "NV12");
    expect(c).toEqual({ depth: 8, primaries: "BT.709", transfer: "sRGB" });
    expect(colourText(c)).toBe("8-bit sRGB, BT.709 primaries");
  });

  test("an HDR10 frame reads as 10-bit PQ with BT.2020 primaries", () => {
    const c = videoFrameColour({ primaries: "bt2020", transfer: "pq" }, "P010");
    expect(colourText(c)).toBe("10-bit PQ, BT.2020 primaries");
  });

  test("a GPU-backed 10-bit frame has no format, so its depth comes from the codec", () => {
    expect(videoFrameColour({ primaries: "bt2020", transfer: "pq" }, null, 10).depth).toBe(10);
    expect(videoFrameColour({ primaries: "bt709", transfer: "bt709" }, null).depth).toBeNull();
  });

  test("names WebCodecs adds later pass through as they are", () => {
    expect(videoFrameColour({ primaries: "jedec22", transfer: "smpte240m" }, "NV12")).toEqual({
      depth: 8,
      primaries: "jedec22",
      transfer: "smpte240m",
    });
  });

  test("PyroWave's sequence header codes", () => {
    expect(colourText(pyroColour({ primaries: 0, transfer: 0 }))).toBe("8-bit BT.709, BT.709 primaries");
    expect(colourText(pyroColour({ primaries: 1, transfer: 1 }))).toBe("8-bit PQ, BT.2020 primaries");
    expect(colourText(pyroColour(null))).toBe("8-bit");
  });

  test("the 10-bit codec names say so, today's are 8-bit", () => {
    expect(codecDepth("hevc-10")).toBe(10);
    expect(codecDepth("av1-10")).toBe(10);
    for (const c of ["h264", "hevc", "av1", "pyrowave420", "pyrowave444"]) expect(codecDepth(c)).toBe(8);
  });

  test("nothing known is no text", () => {
    expect(colourText(null)).toBeNull();
    expect(colourText(videoFrameColour(null, null))).toBeNull();
  });
});

describe("the screen", () => {
  test("the display's text", () => {
    expect(displayText(true, 120)).toBe("120 Hz, HDR");
    expect(displayText(false, 60)).toBe("60 Hz, SDR");
    expect(displayText(null, 120)).toBe("120 Hz");
    expect(displayText(null, null)).toBeNull();
  });
});

describe("the display line the streamer hears", () => {
  test("what a browser can tell", () => {
    expect(displayLine(true, 120)).toEqual({ t: "display", hdr: true, refreshHz: 120 });
    expect(displayLine(false, null)).toEqual({ t: "display", hdr: false });
    expect(displayLine(null, 60)).toEqual({ t: "display", refreshHz: 60 });
  });

  test("nothing to tell, nothing sent", () => {
    expect(displayLine(null, null)).toBeNull();
  });
});
