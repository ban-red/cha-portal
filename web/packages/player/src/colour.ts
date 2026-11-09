// What colour a picture arrives in and what the screen can show, for the stats panel: the first
// record HDR work needs (docs/plans/hdr-and-vrr.md, step 1.2). Pure, so the names are tested.

/** A picture's colour, as far as the decoder or the stream says. Null where it doesn't. */
export interface PictureColour {
  depth: 8 | 10 | null;
  primaries: string | null;
  transfer: string | null;
}

const PRIMARIES: Record<string, string> = {
  bt709: "BT.709",
  bt470bg: "BT.601",
  smpte170m: "BT.601",
  bt2020: "BT.2020",
  smpte432: "P3",
};

const TRANSFERS: Record<string, string> = {
  bt709: "BT.709",
  smpte170m: "BT.709",
  "iec61966-2-1": "sRGB",
  linear: "linear",
  pq: "PQ",
  hlg: "HLG",
};

/** Ten-bit pixel formats WebCodecs names; the 8-bit ones are the rest it knows. */
const TEN_BIT = new Set(["P010", "I420P10", "I420AP10", "I422P10", "I422AP10", "I444P10", "I444AP10"]);
const EIGHT_BIT = new Set(["NV12", "I420", "I420A", "I422", "I422A", "I444", "I444A", "RGBA", "RGBX", "BGRA", "BGRX"]);

/**
 * A decoded WebCodecs frame's colour. A GPU-backed 10-bit frame has no `format` (it can't be read
 * back), so its depth comes from `codecDepth`, the depth the stream's codec string asks for.
 */
export function videoFrameColour(
  colorSpace: { primaries?: string | null; transfer?: string | null } | null | undefined,
  format: string | null | undefined,
  codecDepth: 8 | 10 | null = null,
): PictureColour {
  const depth = format && TEN_BIT.has(format) ? 10 : format && EIGHT_BIT.has(format) ? 8 : codecDepth;
  return {
    depth,
    primaries: named(PRIMARIES, colorSpace?.primaries),
    transfer: named(TRANSFERS, colorSpace?.transfer),
  };
}

/** A PyroWave sequence header's colour (`ColorInfo`: 0/1 codes). PyroWave's planes are 8-bit today. */
export function pyroColour(c: { primaries: number; transfer: number } | null | undefined): PictureColour {
  if (!c) return { depth: 8, primaries: null, transfer: null };
  return { depth: 8, primaries: c.primaries ? "BT.2020" : "BT.709", transfer: c.transfer ? "PQ" : "BT.709" };
}

/** "8-bit sRGB, BT.709 primaries"; null when nothing is known. */
export function colourText(c: PictureColour | null): string | null {
  if (!c || (!c.depth && !c.transfer && !c.primaries)) return null;
  const curve = [c.depth ? `${c.depth}-bit` : null, c.transfer].filter(Boolean).join(" ");
  return [curve || null, c.primaries ? `${c.primaries} primaries` : null].filter(Boolean).join(", ");
}

/** "120 Hz, HDR": what the stats say about this screen. Null when neither is known. */
export function displayText(hdr: boolean | null, refreshHz: number | null): string | null {
  const parts = [refreshHz ? `${refreshHz} Hz` : null, hdr === null ? null : hdr ? "HDR" : "SDR"];
  return parts.some(Boolean) ? parts.filter(Boolean).join(", ") : null;
}

/** The bit depth a codec name asks for: the 10-bit names end in `-10` (docs/plans/hdr-and-vrr.md, 6.1). */
export function codecDepth(codec: string): 8 | 10 {
  return codec.endsWith("-10") ? 10 : 8;
}

/**
 * The `display` line a page sends the streamer (docs/plans/hdr-and-vrr.md, decision 5): what
 * this screen can show, as far as a browser can tell. Null when it can tell nothing.
 */
export function displayLine(hdr: boolean | null, refreshHz: number | null): Record<string, unknown> | null {
  if (hdr === null && !refreshHz) return null;
  return { t: "display", ...(hdr === null ? {} : { hdr }), ...(refreshHz ? { refreshHz } : {}) };
}

/** Whether the screen can show HDR (`dynamic-range: high`); null where the browser can't say. */
export function dynamicRangeHigh(): boolean | null {
  if (typeof matchMedia !== "function") return null;
  return matchMedia("(dynamic-range: high)").matches;
}

function named(names: Record<string, string>, value: string | null | undefined): string | null {
  if (!value) return null;
  return names[value] ?? value;
}
