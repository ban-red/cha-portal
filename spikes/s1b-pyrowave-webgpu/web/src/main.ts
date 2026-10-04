import {
  PyroWaveDecoder,
  PyroWaveDevice,
  type PyroWaveFile,
  YuvRenderer,
  parsePyroWaveFile,
} from "@cha/pyrowave-webgpu";
import { GpuTimer, type StageTimes } from "./timer";

interface ClipInfo {
  name: string;
  file: string;
  ref: string;
  width: number;
  height: number;
  chroma: "420" | "444";
  frames: number;
}

interface Loaded {
  info: ClipInfo;
  file: PyroWaveFile;
  decoder: PyroWaveDecoder;
  colorSet: boolean;
}

interface Pct {
  p50: number | null;
  p95: number | null;
}

interface Row {
  clip: string;
  mode: "validate" | "throughput" | "paced";
  /** Target rate for paced runs. */
  fps: number | null;
  frames: number;
  parseMs: Pct;
  dequantMs: Pct;
  idwtMs: Pct;
  gpuTotalMs: Pct;
  submitToDoneMs: Pct;
  wallMsPerFrame: number | null;
  check: { pass: boolean; detail: string } | null;
}

interface Submitted {
  parseMs: number;
  submittedAt: number;
  done: Promise<number>;
  gpu: Promise<StageTimes> | null;
}

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const canvas = $<HTMLCanvasElement>("canvas");
const rows: Row[] = [];
let env: Record<string, unknown> = {};
let clips: ClipInfo[] = [];
let pw: PyroWaveDevice;
let ctx: GPUCanvasContext;
let renderer: YuvRenderer;
let timer: GpuTimer | null = null;
let loaded: Loaded | null = null;
let busy = false;

async function init(): Promise<void> {
  if (!navigator.gpu) return status("WebGPU is not available in this browser.");
  const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
  if (!adapter) return status("No WebGPU adapter.");
  env = {
    userAgent: navigator.userAgent,
    adapter: { vendor: adapter.info.vendor, architecture: adapter.info.architecture, description: adapter.info.description },
    subgroups: adapter.features.has("subgroups"),
    subgroupSizes: [adapter.info.subgroupMinSize, adapter.info.subgroupMaxSize],
    timestampQuery: adapter.features.has("timestamp-query"),
  };
  $("env").textContent = JSON.stringify(env, null, 2);
  const support = PyroWaveDevice.supports(adapter);
  if (!support.ok) return status(`Cannot decode PyroWave here: ${support.reason}`);

  pw = await PyroWaveDevice.create(adapter);
  pw.device.lost.then((info) => status(`GPU device lost: ${info.message}`));
  pw.device.addEventListener("uncapturederror", (e) => {
    const message = (e as GPUUncapturedErrorEvent).error.message;
    console.error(message);
    status(`GPU error: ${message}`);
  });
  timer = pw.timestamps ? new GpuTimer(pw.device) : null;
  ctx = canvas.getContext("webgpu")!;
  const format = navigator.gpu.getPreferredCanvasFormat();
  ctx.configure({ device: pw.device, format, alphaMode: "opaque" });
  renderer = new YuvRenderer(pw.device, format);

  clips = (await (await fetch("clips/index.json")).json()) as ClipInfo[];
  const select = $<HTMLSelectElement>("clip");
  for (const c of clips) select.add(new Option(`${c.name} (${c.width}x${c.height} ${c.chroma}, ${c.frames} frames)`, c.name));

  const selected = () => clips.find((c) => c.name === select.value)!;
  $("validate").onclick = () => guarded(async () => void (await validate(await load(selected()))));
  $("throughput").onclick = () => guarded(async () => void (await throughput(await load(selected()))));
  $("paced").onclick = () => guarded(async () => void (await paced(await load(selected()), seconds(), rate())));
  $("all").onclick = () =>
    guarded(async () => {
      for (const c of clips) {
        const l = await load(c);
        await validate(l);
        await throughput(l);
        await paced(l, seconds(), rate());
      }
      status("All clips done. Use “Copy results JSON” to save them.");
    });
  $("copy").onclick = async () => {
    await navigator.clipboard.writeText(JSON.stringify({ capturedAt: new Date().toISOString(), env, rows }, null, 2));
    status(`Copied ${rows.length} result(s).`);
  };
  status(`Ready: ${adapter.info.vendor} ${adapter.info.architecture}, timestamps ${timer ? "on" : "off"}.`);
}

const seconds = () => Number($<HTMLInputElement>("secs").value) || 10;
const rate = () => Number($<HTMLInputElement>("fps").value) || 60;

async function guarded(task: () => Promise<void>): Promise<void> {
  if (busy) return;
  busy = true;
  document.querySelectorAll("button").forEach((b) => (b.disabled = b.id !== "copy"));
  try {
    await task();
  } catch (err) {
    console.error(err);
    status(`Error: ${String(err)}`);
  } finally {
    busy = false;
    document.querySelectorAll("button").forEach((b) => (b.disabled = false));
  }
}

async function load(info: ClipInfo): Promise<Loaded> {
  if (loaded?.info.name === info.name) return loaded;
  status(`Loading ${info.file}…`);
  loaded?.decoder.destroy();
  const file = parsePyroWaveFile(await (await fetch(`clips/${info.file}`)).arrayBuffer());
  const decoder = new PyroWaveDecoder(pw, file.width, file.height, file.chroma);
  canvas.width = file.width;
  canvas.height = file.height;
  loaded = { info, file, decoder, colorSet: false };
  return loaded;
}

/** Parses frame `index`, records decode (and optionally a draw) and submits it. */
function submitFrame(l: Loaded, index: number, render: boolean): Submitted {
  const t0 = performance.now();
  // Benchmarks replay frames out of sequence order, so start each one clean.
  l.decoder.clear();
  if (!l.decoder.pushPacket(l.file.frames[index]!)) throw new Error(`frame ${index} is corrupt`);
  if (!l.decoder.isReady()) throw new Error(`frame ${index} is incomplete`);
  if (!l.colorSet) {
    renderer.setSource(l.decoder.planes, l.decoder.parser.color);
    l.colorSet = true;
  }
  const t1 = performance.now();

  const cmd = pw.device.createCommandEncoder();
  l.decoder.encode(cmd, timer ? { querySet: timer.querySet, first: 0 } : undefined);
  const target = timer?.resolve(cmd);
  if (render) renderer.draw(cmd, ctx.getCurrentTexture().createView(), canvas.width, canvas.height);
  pw.device.queue.submit([cmd.finish()]);
  const submittedAt = performance.now();
  return {
    parseMs: t1 - t0,
    submittedAt,
    done: pw.device.queue.onSubmittedWorkDone().then(() => performance.now()),
    gpu: target && timer ? timer.read(target) : null,
  };
}

/** Decodes frame 0 and compares it with the native decoder's output. */
async function validate(l: Loaded): Promise<Row> {
  status(`Validating ${l.info.name}…`);
  const s = submitFrame(l, 0, true);
  await s.done;
  const planes = l.decoder.planes;
  const read = pw.device.createBuffer({ size: planes.buffer.size, usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
  const cmd = pw.device.createCommandEncoder();
  cmd.copyBufferToBuffer(planes.buffer, 0, read, 0, planes.buffer.size);
  pw.device.queue.submit([cmd.finish()]);
  await read.mapAsync(GPUMapMode.READ);
  const gpu = new Uint8Array(read.getMappedRange());
  const ref = new Uint8Array(await (await fetch(`clips/${l.info.ref}`)).arrayBuffer());

  let refOffset = 0;
  let maxDiff = 0;
  let mismatched = 0;
  let total = 0;
  for (let p = 0; p < 3; p++) {
    const w = l.decoder.layout.planeWidth(p);
    const h = l.decoder.layout.planeHeight(p);
    for (let y = 0; y < h; y++) {
      const row = planes.offsets[p]! * 4 + y * planes.strides[p]! * 4;
      for (let x = 0; x < w; x++) {
        const d = Math.abs(gpu[row + x]! - ref[refOffset + y * w + x]!);
        if (d) {
          mismatched++;
          if (d > maxDiff) maxDiff = d;
        }
      }
    }
    refOffset += w * h;
    total += w * h;
  }
  read.unmap();
  read.destroy();

  const pct = (mismatched / total) * 100;
  const detail =
    maxDiff === 0 ? "bit-exact vs native" : `max diff ${maxDiff}, ${pct.toFixed(3)}% of samples differ`;
  const row = summarize(l, "validate", [await sample(s)], null, { pass: maxDiff <= 1, detail });
  status(`${l.info.name}: ${detail}`);
  return row;
}

/** Back-to-back decodes with a few frames in flight. */
async function throughput(l: Loaded): Promise<Row> {
  status(`Throughput ${l.info.name}…`);
  const frames = l.file.frames.length;
  for (let i = 0; i < 10; i++) await submitFrame(l, i % frames, false).done; // warm up
  const total = frames * 3;
  const inFlight: Submitted[] = [];
  const all: Submitted[] = [];
  const start = performance.now();
  for (let i = 0; i < total; i++) {
    if (inFlight.length >= 3) await inFlight.shift()!.done;
    const s = submitFrame(l, i % frames, false);
    inFlight.push(s);
    all.push(s);
  }
  const end = await all.at(-1)!.done;
  const samples = await Promise.all(all.map(sample));
  return summarize(l, "throughput", samples, (end - start) / total, null);
}

/** Decodes and draws at `fps` on the display's rAF clock, like a stream would. */
async function paced(l: Loaded, secs: number, fps: number): Promise<Row> {
  status(`Paced ${fps} fps ${l.info.name} for ${secs}s…`);
  const frames = l.file.frames.length;
  const interval = 1000 / fps;
  const all: Submitted[] = [];
  await new Promise<void>((resolve, reject) => {
    const start = performance.now();
    let next = start;
    let index = 0;
    const tick = (now: number) => {
      try {
        if (now - start >= secs * 1000) return resolve();
        if (now >= next) {
          all.push(submitFrame(l, index++ % frames, true));
          next += interval;
          if (next < now) next = now + interval;
        }
        requestAnimationFrame(tick);
      } catch (err) {
        reject(err);
      }
    };
    requestAnimationFrame(tick);
  });
  const samples = await Promise.all(all.map(sample));
  return summarize(l, "paced", samples, null, null, fps);
}

interface Sample {
  parseMs: number;
  submitToDoneMs: number;
  gpu: StageTimes | null;
}

async function sample(s: Submitted): Promise<Sample> {
  const [done, gpu] = await Promise.all([s.done, s.gpu]);
  return { parseMs: s.parseMs, submitToDoneMs: done - s.submittedAt, gpu };
}

function summarize(
  l: Loaded,
  mode: Row["mode"],
  samples: Sample[],
  wallMsPerFrame: number | null,
  check: Row["check"],
  fps: number | null = null,
): Row {
  const gpu = samples.map((s) => s.gpu).filter((g): g is StageTimes => g !== null);
  const row: Row = {
    clip: l.info.name,
    mode,
    fps,
    frames: samples.length,
    parseMs: pct(samples.map((s) => s.parseMs)),
    dequantMs: pct(gpu.map((g) => g.dequantMs)),
    idwtMs: pct(gpu.map((g) => g.idwtMs)),
    gpuTotalMs: pct(gpu.map((g) => g.totalMs)),
    submitToDoneMs: pct(samples.map((s) => s.submitToDoneMs)),
    wallMsPerFrame,
    check,
  };
  rows.push(row);
  render(row);
  return row;
}

function pct(values: number[]): Pct {
  if (!values.length) return { p50: null, p95: null };
  const sorted = [...values].sort((a, b) => a - b);
  const at = (q: number) => sorted[Math.min(sorted.length - 1, Math.ceil(q * sorted.length) - 1)]!;
  return { p50: at(0.5), p95: at(0.95) };
}

function render(row: Row): void {
  const f = (v: number | null) => (v === null ? "–" : v.toFixed(2));
  const pair = (p: Pct) => `${f(p.p50)} / ${f(p.p95)}`;
  const tr = document.createElement("tr");
  const cells = [
    row.clip,
    row.fps ? `${row.mode} ${row.fps}` : row.mode,
    String(row.frames),
    pair(row.parseMs),
    pair(row.dequantMs),
    pair(row.idwtMs),
    pair(row.gpuTotalMs),
    pair(row.submitToDoneMs),
    f(row.wallMsPerFrame),
  ];
  for (const c of cells) {
    const td = document.createElement("td");
    td.textContent = c;
    tr.append(td);
  }
  const check = document.createElement("td");
  if (row.check) {
    check.className = row.check.pass ? "pass" : "fail";
    check.textContent = row.check.pass ? "pass" : "fail";
    check.title = row.check.detail;
  } else {
    check.textContent = "–";
  }
  tr.append(check);
  $("results").append(tr);
}

function status(text: string): void {
  $("status").textContent = text;
}

void init().catch((err) => status(`Init failed: ${String(err)}`));
