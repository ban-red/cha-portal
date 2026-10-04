// The browser's side of cha-proto's FEC (crates/cha-proto/src/fec.rs, plan
// §3.1 rule 6): a systematic Reed-Solomon erasure code over GF(2⁸) with a
// Cauchy matrix. A frame's data fragments come in blocks of up to BLOCK;
// each block has `m` parity fragments, and any `k` of a block's `k + m`
// shards rebuild it. Only recovery runs here (encode is for the check
// against the Rust side's pinned vector).

/** Data shards per block, at most. */
export const BLOCK = 128;

const EXP = new Uint8Array(512);
const LOG = new Uint8Array(256);
{
  // x⁸ + x⁴ + x³ + x² + 1
  let x = 1;
  for (let i = 0; i < 255; i++) {
    EXP[i] = x;
    LOG[x] = i;
    x <<= 1;
    if (x & 0x100) x ^= 0x11d;
  }
  for (let i = 255; i < 512; i++) EXP[i] = EXP[i - 255]!;
}

function mul(a: number, b: number): number {
  return a === 0 || b === 0 ? 0 : EXP[LOG[a]! + LOG[b]!]!;
}

function inv(a: number): number {
  return EXP[255 - LOG[a]!]!;
}

/** Parity row `r`'s coefficient for data shard `i` (of `m` rows). */
function coefficient(r: number, i: number, m: number): number {
  return inv((r ^ (m + i)) & 0xff);
}

/** acc ^= c · src (src shorter: zeros after it). */
function mulAdd(acc: Uint8Array, src: Uint8Array, c: number): void {
  if (c === 0) return;
  const lc = LOG[c]!;
  const n = Math.min(acc.length, src.length);
  for (let j = 0; j < n; j++) {
    const s = src[j]!;
    if (s !== 0) acc[j]! ^= EXP[lc + LOG[s]!]!;
  }
}

/** The `m` parity shards of one block (for checks; the streamer encodes). */
export function encode(data: Uint8Array[], m: number, shardLen: number): Uint8Array[] {
  return Array.from({ length: m }, (_, r) => {
    const parity = new Uint8Array(shardLen);
    data.forEach((shard, i) => mulAdd(parity, shard, coefficient(r, i, m)));
    return parity;
  });
}

/**
 * Rebuilds a block's missing data shards in place (`shardLen` long,
 * zero-padded) from its parity shards (`parity[r]`: row r, if it arrived).
 * False if too few arrived.
 */
export function recover(data: (Uint8Array | undefined)[], parity: (Uint8Array | undefined)[], shardLen: number): boolean {
  // Index loops throughout: arrays of fragments can be sparse, and
  // forEach/map/flatMap skip holes.
  const m = parity.length;
  const missing: number[] = [];
  for (let i = 0; i < data.length; i++) if (!data[i]) missing.push(i);
  if (!missing.length) return true;
  const rows: number[] = [];
  for (let r = 0; r < m && rows.length < missing.length; r++) if (parity[r]) rows.push(r);
  if (rows.length < missing.length) return false;
  const e = missing.length;
  // Each row: the parity minus what the shards we have contribute.
  const syndromes = rows.map((r) => {
    const s = new Uint8Array(shardLen);
    s.set(parity[r]!.subarray(0, shardLen));
    for (let i = 0; i < data.length; i++) {
      const shard = data[i];
      if (shard) mulAdd(s, shard, coefficient(r, i, m));
    }
    return s;
  });
  // Gauss–Jordan on the e × e Cauchy system, row operations on the syndromes too.
  const a = rows.map((r) => missing.map((i) => coefficient(r, i, m)));
  for (let col = 0; col < e; col++) {
    let pivot = col;
    while (pivot < e && a[pivot]![col] === 0) pivot++;
    if (pivot === e) return false;
    [a[col], a[pivot]] = [a[pivot]!, a[col]!];
    [syndromes[col], syndromes[pivot]] = [syndromes[pivot]!, syndromes[col]!];
    const scale = inv(a[col]![col]!);
    a[col] = a[col]!.map((v) => mul(v, scale));
    syndromes[col] = syndromes[col]!.map((v) => mul(v, scale));
    for (let other = 0; other < e; other++) {
      const factor = a[other]![col]!;
      if (other === col || factor === 0) continue;
      a[other] = a[other]!.map((v, j) => v ^ mul(factor, a[col]![j]!));
      mulAdd(syndromes[other]!, syndromes[col]!, factor);
    }
  }
  missing.forEach((i, j) => (data[i] = syndromes[j]));
  return true;
}
