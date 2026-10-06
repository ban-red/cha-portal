import type { TrafficConfig } from "./proto";
import type { RunProgress, RunResult, TransportKind } from "./run";
import { runWebRtc } from "./webrtc";
import type { WtEvent, WtStart } from "./worker";

interface Preset {
  id: string;
  label: string;
  mbps: number;
  fps: number;
  gate?: boolean;
}

// Bitrates from PyroWave's viewing-distance model ("transparent"), see docs/research/02.
const PRESETS: Preset[] = [
  { id: "1080p60-420", label: "1080p60 4:2:0 couch (≈220)", mbps: 220, fps: 60 },
  { id: "1440p60-420", label: "1440p60 4:2:0 couch (≈290)", mbps: 290, fps: 60 },
  { id: "1440p60-444", label: "1440p60 4:4:4 desk (≈590) · gate", mbps: 590, fps: 60, gate: true },
  { id: "1440p120-420", label: "1440p120 4:2:0 couch (≈580)", mbps: 580, fps: 120 },
  { id: "stress-800", label: "Stress 800 Mbit/s @ 60", mbps: 800, fps: 60 },
  { id: "custom", label: "Custom", mbps: 100, fps: 60 },
];
const SWEEP = ["1080p60-420", "1440p60-420", "1440p60-444", "1440p120-420", "stress-800"];
/** Usable goodput of the baseline wired 1 GbE link, for serialization-time math. */
const LINK_MBPS = 940;

interface ServerInfo {
  wt_port: number;
  cert_hash_hex: string;
  addresses: string[];
  cc: string;
}

interface Row {
  presetId: string;
  presetLabel: string;
  wireMs: number;
  /** Receiver options in effect, so exported runs are comparable. */
  client: { byob: boolean; highWaterMark: number };
  result: RunResult;
  gate: { pass: boolean; failures: string[] };
}

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const rows: Row[] = [];
let env: Record<string, unknown> = {};
let lastInfo: ServerInfo | null = null;
let busy = false;

function setup(): void {
  const presetSel = $<HTMLSelectElement>("preset");
  for (const p of PRESETS) presetSel.add(new Option(p.label, p.id));
  presetSel.value = "1440p60-444";
  const applyPreset = () => {
    const p = PRESETS.find((x) => x.id === presetSel.value);
    if (p && p.id !== "custom") {
      $<HTMLInputElement>("mbps").value = String(p.mbps);
      $<HTMLInputElement>("fps").value = String(p.fps);
    }
  };
  presetSel.onchange = applyPreset;
  applyPreset();
  restoreInputs();

  $("run").onclick = () => void guarded(async () => {
    const transport = $<HTMLSelectElement>("transport").value as TransportKind;
    await runOne(presetSel.value, readConfig(), transport);
  });
  $("sweep").onclick = () => void guarded(async () => {
    const secs = Number($<HTMLInputElement>("secs").value) || 15;
    for (const id of SWEEP) {
      const p = PRESETS.find((x) => x.id === id)!;
      for (const transport of ["webtransport", "webrtc"] as const) {
        const config = { mbps: p.mbps, fps: p.fps, secs, dgram: readConfig().dgram };
        await runOne(id, config, transport);
        await sleep(2000);
      }
    }
    status("Sweep complete. Use “Copy results JSON” to save it.");
  });
  $("copy").onclick = async () => {
    // The server's addresses and certificate hash identify the machine; results don't need them.
    const server = lastInfo && { ...lastInfo, addresses: undefined, cert_hash_hex: undefined };
    const payload = { capturedAt: new Date().toISOString(), env, server, rows };
    await navigator.clipboard.writeText(JSON.stringify(payload, null, 2));
    status(`Copied ${rows.length} result(s) to the clipboard.`);
  };
  void describeEnvironment();
}

async function guarded(task: () => Promise<void>): Promise<void> {
  if (busy) return;
  busy = true;
  for (const id of ["run", "sweep"]) $<HTMLButtonElement>(id).disabled = true;
  saveInputs();
  try {
    await task();
  } catch (err) {
    status(`Error: ${String(err)}`);
  } finally {
    busy = false;
    for (const id of ["run", "sweep"]) $<HTMLButtonElement>(id).disabled = false;
  }
}

function readConfig(): TrafficConfig {
  return {
    mbps: Number($<HTMLInputElement>("mbps").value),
    fps: Number($<HTMLInputElement>("fps").value),
    secs: Number($<HTMLInputElement>("secs").value),
    dgram: Number($<HTMLInputElement>("dgram").value),
  };
}

async function runOne(presetId: string, config: TrafficConfig, transport: TransportKind): Promise<void> {
  const host = $<HTMLInputElement>("host").value.trim();
  const httpPort = Number($<HTMLInputElement>("httpPort").value);
  const preset = PRESETS.find((p) => p.id === presetId);
  const label = preset && preset.id !== "custom" ? preset.label : `${config.mbps} Mbit/s @ ${config.fps}`;
  status(`Running ${transport} · ${label} for ${config.secs}s…`);

  let result: RunResult;
  if (transport === "webtransport") {
    const info = await fetchInfo(host, httpPort);
    result = await runWebTransport(info, host, config);
  } else {
    lastInfo ??= await fetchInfo(host, httpPort).catch(() => null);
    result = await runWebRtc({ host, httpPort, config }, renderLive);
  }

  const frameBytes = (config.mbps * 1e6) / 8 / config.fps;
  const wireMs = (frameBytes * 8) / (LINK_MBPS * 1e3);
  const row: Row = {
    presetId,
    presetLabel: label,
    wireMs,
    client: {
      byob: $<HTMLInputElement>("byob").checked,
      highWaterMark: Number($<HTMLInputElement>("hwm").value) || 4096,
    },
    result,
    gate: evaluateGate(result, wireMs),
  };
  rows.push(row);
  renderRow(row);
  status(`Finished ${transport} · ${label} (${result.endReason}).`);
}

async function fetchInfo(host: string, httpPort: number): Promise<ServerInfo> {
  const h = host.includes(":") ? `[${host}]` : host;
  const res = await fetch(`http://${h}:${httpPort}/info`);
  if (!res.ok) throw new Error(`GET /info failed: ${res.status}`);
  lastInfo = (await res.json()) as ServerInfo;
  return lastInfo;
}

function runWebTransport(info: ServerInfo, host: string, config: TrafficConfig): Promise<RunResult> {
  const worker = new Worker(new URL("./worker.ts", import.meta.url), { type: "module" });
  const start: WtStart = {
    type: "start",
    host,
    wtPort: info.wt_port,
    certHashHex: info.cert_hash_hex,
    config,
    byob: $<HTMLInputElement>("byob").checked,
    highWaterMark: Number($<HTMLInputElement>("hwm").value) || 4096,
  };
  return new Promise<RunResult>((resolve, reject) => {
    worker.onmessage = (e: MessageEvent<WtEvent>) => {
      const msg = e.data;
      if (msg.type === "progress") renderLive(msg.progress);
      else if (msg.type === "result") resolve(msg.result);
      else reject(new Error(msg.message));
    };
    worker.onerror = (e) => reject(new Error(e.message));
    worker.postMessage(start);
  }).finally(() => worker.terminate());
}

/**
 * Gate (docs/PLAN.md, spike S1). Frame completion is judged against the time
 * the frame needs on the wire, because a 1.2 MB frame cannot arrive faster
 * than 1 GbE serializes it, natively or in a browser.
 */
function evaluateGate(r: RunResult, wireMs: number): Row["gate"] {
  const s = r.summary;
  const failures: string[] = [];
  const target = r.config.mbps;
  const median = s.rateMbps.median ?? 0;
  if (median < target * 0.97) failures.push(`recv ${median.toFixed(0)} < 97% of ${target}`);
  if (s.datagramLoss >= 0.005) failures.push(`loss ${(s.datagramLoss * 100).toFixed(2)}% ≥ 0.5%`);
  const total = s.framesComplete + s.framesIncomplete + s.framesMissing;
  const okRatio = total ? s.framesComplete / total : 0;
  if (okRatio < 0.995) failures.push(`frames ok ${(okRatio * 100).toFixed(2)}% < 99.5%`);
  const spread = s.spreadMs.p99 ?? Infinity;
  if (spread > wireMs + 3) failures.push(`spread p99 ${spread.toFixed(1)} > wire ${wireMs.toFixed(1)} + 3 ms`);
  const done = s.latencyCompleteMs.p99;
  if (done !== null && done > wireMs + 6) failures.push(`latency p99 ${done.toFixed(1)} > wire + 6 ms`);
  const dropped = r.server?.frames_dropped_backpressure ?? 0;
  if (dropped > 0) failures.push(`${dropped} frames dropped at sender`);
  return { pass: failures.length === 0, failures };
}

function renderLive(p: RunProgress): void {
  const s = p.snapshot;
  const srv = p.server;
  const cells: [string, string][] = [
    ["Elapsed", `${(s.elapsedMs / 1000).toFixed(1)} s`],
    ["Receive rate", `${s.rateMbps.toFixed(0)} Mbit/s`],
    ["Datagrams/s", s.datagramsPerSec.toFixed(0)],
    ["Frames complete", String(s.framesComplete)],
    ["Frames incomplete", String(s.framesIncomplete)],
    ["In flight", String(s.framesInFlight)],
    ["Server frames sent", String(srv?.frames_sent ?? "–")],
    ["Sender drops", String(srv?.frames_dropped_backpressure ?? "–")],
    ["Server RTT", srv?.rtt_ms != null ? `${srv.rtt_ms.toFixed(2)} ms` : "–"],
    ["Ping RTT (min)", p.minRttMs != null ? `${p.minRttMs.toFixed(2)} ms` : "–"],
    ["QUIC cwnd", srv?.cwnd != null ? `${(srv.cwnd / 1024).toFixed(0)} KiB` : "–"],
    ["SCTP buffered", srv?.buffered_amount != null ? `${(srv.buffered_amount / 1024).toFixed(0)} KiB` : "–"],
  ];
  $("live").innerHTML = cells
    .map(([k, v]) => `<div class="stat"><b>${v}</b><span>${k}</span></div>`)
    .join("");
}

function renderRow(row: Row): void {
  const s = row.result.summary;
  const total = s.framesComplete + s.framesIncomplete + s.framesMissing;
  const fmt = (v: number | null, d = 1) => (v === null ? "–" : v.toFixed(d));
  const pair = (a: number | null, b: number | null) => `${fmt(a)} / ${fmt(b)}`;
  const tr = document.createElement("tr");
  const cells = [
    row.result.transport,
    row.presetLabel,
    `${row.result.config.mbps}`,
    fmt(s.rateMbps.median, 0),
    fmt(s.rateMbps.min, 0),
    `${(s.datagramLoss * 100).toFixed(3)}%`,
    total ? `${((s.framesComplete / total) * 100).toFixed(2)}%` : "–",
    `${pair(s.spreadMs.p50, s.spreadMs.p99)} (wire ${row.wireMs.toFixed(1)})`,
    pair(s.latencyFirstMs.p50, s.latencyFirstMs.p99),
    pair(s.latencyCompleteMs.p50, s.latencyCompleteMs.p99),
    fmt(s.completionJitterMs.p99),
    String(row.result.server?.frames_dropped_backpressure ?? "–"),
  ];
  for (const c of cells) {
    const td = document.createElement("td");
    td.textContent = c;
    tr.append(td);
  }
  const gate = document.createElement("td");
  gate.className = row.gate.pass ? "pass" : "fail";
  gate.textContent = row.gate.pass ? "pass" : "fail";
  gate.title = row.gate.failures.join("\n");
  tr.append(gate);
  $("results").append(tr);
}

async function describeEnvironment(): Promise<void> {
  const nav = navigator as Navigator & { userAgentData?: { brands: unknown; platform: string } };
  env = {
    userAgent: nav.userAgent,
    brands: nav.userAgentData?.brands,
    platform: nav.userAgentData?.platform,
    hardwareConcurrency: nav.hardwareConcurrency,
    webTransport: typeof WebTransport !== "undefined",
    crossOriginIsolated,
  };
  try {
    const gpu = (navigator as Navigator & { gpu?: GPU }).gpu;
    const adapter = await gpu?.requestAdapter();
    if (adapter) {
      env.webgpu = {
        vendor: adapter.info.vendor,
        architecture: adapter.info.architecture,
        subgroups: adapter.features.has("subgroups"),
        shaderF16: adapter.features.has("shader-f16"),
        maxComputeWorkgroupStorageSize: adapter.limits.maxComputeWorkgroupStorageSize,
        maxStorageBufferBindingSize: adapter.limits.maxStorageBufferBindingSize,
      };
    } else {
      env.webgpu = null;
    }
  } catch (err) {
    env.webgpu = `error: ${String(err)}`;
  }
  $("env").textContent = JSON.stringify(env, null, 2);
}

function status(text: string): void {
  $("status").textContent = text;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

const STORED = ["host", "httpPort", "secs", "dgram", "hwm", "transport"];
function saveInputs(): void {
  try {
    const values = Object.fromEntries(STORED.map((id) => [id, $<HTMLInputElement>(id).value]));
    localStorage.setItem("s1-inputs", JSON.stringify(values));
  } catch {
    // Storage unavailable; inputs just won't persist.
  }
}
function restoreInputs(): void {
  try {
    const raw = localStorage.getItem("s1-inputs");
    if (!raw) return;
    const values = JSON.parse(raw) as Record<string, string>;
    for (const id of STORED) if (values[id] !== undefined) $<HTMLInputElement>(id).value = values[id];
  } catch {
    // Ignore corrupt or unavailable storage.
  }
}

setup();
