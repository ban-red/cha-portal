// The browser's readings as the stats panel's value keys (web/packages/ui-spec/stats-panel.json).
// This is the only place the panel's names meet the player's StatsSnapshot. A key left out is one
// the browser doesn't have right now, and the panel reads it as "–" (or hides its row).
import type { NodeStats, StatsSnapshot } from "@cha/player";
import type { PanelValues } from "@cha/ui-spec";

import { codecTag } from "./statsOverlay";

export interface StatsValuesInput {
  stats: StatsSnapshot | null;
  /** The codec chosen, until the stream says what it decodes. */
  codec: string;
  transport: "webtransport" | "webrtc" | "websocket" | null;
  /** Times the stream came back on its own after dropping, this visit. */
  reconnects?: number;
}

const positive = (v: number | null | undefined): number | undefined => (v ? v : undefined);

function nodeValues(n: NodeStats): PanelValues {
  return {
    node_cpu: n.cpu,
    // Only a node that reports its cores has a load line, and only a card with a limit has one.
    node_cores: positive(n.cores),
    node_load1: n.load1,
    node_mem_used: n.memUsed,
    node_mem_total: n.memTotal,
    node_gpu: n.gpu,
    node_vram_used: n.vramUsed,
    node_vram_total: n.vramTotal,
    node_temp: n.temp,
    node_power: n.power,
    node_power_limit: positive(n.powerLimit),
    node_clock: n.clock,
    node_enc: n.enc,
    node_dec: n.dec,
    node_streamer_cpu: n.streamerCpu,
  };
}

export function statsValues({ stats: s, codec, transport, reconnects }: StatsValuesInput): PanelValues {
  const name = s?.codec ?? codec;
  return {
    shown_fps: s?.fps,
    target_fps: positive(s?.targetFps),
    mbps: s?.mbps,
    codec_tag: codecTag(name, transport) || undefined,
    width: s?.width,
    height: s?.height,
    reconnects: positive(reconnects),
    latency_ms: s?.latencyMs,
    decode_ms: s?.decodeMs,
    jitter_buffer_ms: s?.jitterMs,
    audio_jitter_ms: s?.audioJitterMs,
    // The sound output's counters come with WebTransport; the track path has none.
    audio_underruns: s?.audioOutUnderruns,
    rtt_ms: s?.rttMs,
    lost: s?.packetsLost ?? 0,
    // Frames rebuilt from parity are counted by WebTransport only.
    recovered: transport === "webtransport" ? (s?.framesRecovered ?? 0) : undefined,
    dropped: s?.framesDropped ?? 0,
    // PyroWave shows frames from some of their packets; for the others there is nothing to count.
    partial: /pyrowave/i.test(name) || s?.framesPartial ? (s?.framesPartial ?? 0) : undefined,
    ...(s?.node ? nodeValues(s.node) : {}),
  };
}
