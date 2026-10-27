import { expect, test } from "bun:test";

import { encode, recover } from "./fec";

function shards(k: number, len: number, seed: number): Uint8Array[] {
  let x = (Math.imul(seed, 2654435761) | 1) >>> 0;
  return Array.from({ length: k }, (_, i) => {
    // The last shard shorter, as a frame's last fragment is.
    const s = new Uint8Array(i === k - 1 ? Math.floor(len / 3) : len);
    for (let j = 0; j < s.length; j++) {
      x ^= x << 13;
      x >>>= 0;
      x ^= x >>> 17;
      x ^= x << 5;
      x >>>= 0;
      s[j] = x & 0xff;
    }
    return s;
  });
}

test("the parity matches the streamer's (cha-proto's pinned vector)", () => {
  const data = [new Uint8Array([1, 2, 3]), new Uint8Array([4, 5, 6]), new Uint8Array([7, 8])];
  expect(encode(data, 2, 3).map((p) => [...p])).toEqual([
    [177, 0, 141],
    [164, 40, 2],
  ]);
});

test("any k of k + m shards rebuild the block", () => {
  for (const [k, m, len] of [
    [1, 1, 50],
    [5, 2, 64],
    [35, 6, 1180],
    [128, 32, 300],
  ] as const) {
    const data = shards(k, len, k * 31 + m);
    const parity = encode(data, m, len);
    // Lose as many data shards as there is parity, spread over the block.
    const got: (Uint8Array | undefined)[] = data.slice();
    for (let i = 0; i < m; i++) got[Math.floor((i * k) / m)] = undefined;
    expect(recover(got, parity, len)).toBe(true);
    got.forEach((shard, i) => {
      expect([...shard!.subarray(0, data[i]!.length)]).toEqual([...data[i]!]);
      expect(shard!.subarray(data[i]!.length).every((b) => b === 0)).toBe(true);
    });
  }
});

test("lost parity is worked around, and too little is refused", () => {
  const data = shards(20, 300, 11);
  const parity: (Uint8Array | undefined)[] = encode(data, 4, 300);
  parity[0] = undefined;
  parity[2] = undefined;
  const got: (Uint8Array | undefined)[] = data.slice();
  got[5] = undefined;
  got[9] = undefined;
  expect(recover(got, parity, 300)).toBe(true);
  expect([...got[5]!]).toEqual([...data[5]!]);
  const short: (Uint8Array | undefined)[] = data.slice();
  short[1] = short[2] = short[3] = undefined;
  expect(recover(short, parity, 300)).toBe(false);
});
