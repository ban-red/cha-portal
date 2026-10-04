// Spike S7's bench page: the real player against the bench streamer
// (spikes/s7-wan), recording what a viewer would feel each second.
//   ?host=gpu-node.lan&codec=hevc&transport=webtransport
// From the console (or the harness): `await bench.record(70)` → per-second samples.

import { Player, type Codec, type StatsSnapshot } from "@cha/player";

const params = new URLSearchParams(location.search);
const host = params.get("host") ?? "gpu-node.lan";
const codec = (params.get("codec") ?? "hevc") as Codec;
const transport = (params.get("transport") ?? "webtransport") as "webtransport" | "webrtc";
const base = `http://${host}:4515`;
const video = document.querySelector("video")!;
const log = document.getElementById("log")!;

const player = new Player({
  video,
  codec,
  transport,
  muted: true,
  signal: async (offer, c) => {
    const r = await fetch(`${base}/webrtc/media?name=live-${c}&secs=0&token=&host=${host}`, {
      method: "POST",
      body: JSON.stringify(offer),
    });
    if (!r.ok) throw new Error(await r.text());
    return r.json();
  },
  webTransport: async (c) => {
    const info = await fetch(`${base}/info`).then((r) => r.json());
    return { urls: [`https://${host}:${info.wt_port}/media?codec=${c}&token=`], certHash: info.cert_hash_hex };
  },
  onState: (s, detail) => (log.textContent = `${s}${detail ? `: ${detail}` : ""}`),
});

// Frames as the decoder hands them over (a clone of the video track, read
// with a processor): unlike the <video>'s counters, independent of whether
// the page is painted.
// Chromium's breakout-box reader (not in TypeScript's DOM types yet).
declare class MediaStreamTrackProcessor {
  constructor(init: { track: MediaStreamTrack });
  readonly readable: ReadableStream<VideoFrame>;
}

const decoded = { count: 0, at: performance.now() };
async function watchFrames(): Promise<void> {
  const track = (video.srcObject as MediaStream | null)?.getVideoTracks()[0];
  if (!track) return;
  const reader = new MediaStreamTrackProcessor({ track: track.clone() }).readable.getReader();
  for (;;) {
    const { value, done } = await reader.read();
    if (done) return;
    value.close();
    decoded.count++;
    decoded.at = performance.now();
  }
}

interface Sample {
  /** Seconds since recording started. */
  t: number;
  /** Frames decoded this second, and the longest wait between two arrivals
   * (the player's worker clock: true even when this page is hidden). */
  frames: number;
  maxGapMs: number;
  mbps: number | null;
  /** Frames the transport lost this second, and those FEC rebuilt. */
  lost: number;
  recovered: number;
  /** Server send → decoded, p50 / p95 this second (clock-synced). */
  deliveryMs: number | null;
  deliveryP95Ms: number | null;
  rttMs: number | null;
}

async function record(seconds: number): Promise<Sample[]> {
  const samples: Sample[] = [];
  const started = performance.now();
  let last = decoded.count;
  let lastAt = performance.now();
  let lost = (await player.readStats())?.packetsLost ?? 0;
  let recovered = (await player.readStats())?.framesRecovered ?? 0;
  for (let second = 1; second <= seconds; second++) {
    let frames = 0;
    let maxGap = 0;
    while (performance.now() - started < second * 1000) {
      await new Promise((r) => setTimeout(r, 5));
      const n = decoded.count;
      if (n !== last) {
        frames += n - last;
        maxGap = Math.max(maxGap, decoded.at - lastAt);
        last = n;
        lastAt = decoded.at;
      }
    }
    maxGap = Math.max(maxGap, performance.now() - lastAt);
    const s: StatsSnapshot | null = await player.readStats();
    samples.push({
      t: second,
      frames,
      maxGapMs: s?.frameGapMs != null ? Math.round(s.frameGapMs) : Math.round(maxGap),
      mbps: s?.mbps != null ? Math.round(s.mbps * 10) / 10 : null,
      lost: (s?.packetsLost ?? lost) - lost,
      recovered: (s?.framesRecovered ?? recovered) - recovered,
      deliveryMs: s?.deliveryMs != null ? Math.round(s.deliveryMs) : null,
      deliveryP95Ms: s?.deliveryP95Ms != null ? Math.round(s.deliveryP95Ms) : null,
      rttMs: s?.rttMs != null ? Math.round(s.rttMs) : null,
    });
    lost = s?.packetsLost ?? lost;
    recovered = s?.framesRecovered ?? recovered;
    const x = samples[samples.length - 1];
    log.textContent = `${transport} ${codec} t=${x.t}s frames ${x.frames} gap ${x.maxGapMs} ms ${x.mbps} Mbit/s lost ${x.lost} send→decoded ${x.deliveryMs}/${x.deliveryP95Ms} ms`;
  }
  return samples;
}

declare global {
  interface Window {
    bench: { player: Player; record: typeof record; last: Sample[] | null; run: (seconds: number) => void };
  }
}
window.bench = {
  player,
  record,
  last: null,
  // Fire and forget, for harnesses with short timeouts: results in bench.last.
  run: (seconds) => void record(seconds).then((s) => (window.bench.last = s)),
};
await player.connect();
void watchFrames();
