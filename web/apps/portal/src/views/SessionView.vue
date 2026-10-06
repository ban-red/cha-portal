<script setup lang="ts">
import {
  FRAME_RATES,
  Player,
  supportedCodecs,
  supportsPyroWave,
  supportsWebTransport,
  HEALTH_WINDOW,
  assessHealth,
  summarizeRecording,
  isPyroWave,
  hidUnavailableReason,
  type Codec,
  type FrameRate,
  type HealthAssessment,
  type CaptureView,
  type ManagedController,
  type PlayerState,
  type ProbeResult,
  type RecordingSummary,
  type SetupStatus,
  type StatsSnapshot,
  type Transport,
} from "@cha/player";
import { useQuery, useQueryClient } from "@tanstack/vue-query";
import { computed, onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";
import { useRoute } from "vue-router";

import { ApiError, api } from "../api";
import EnvironmentLog from "../components/EnvironmentLog.vue";
import RecordingDialog from "../components/RecordingDialog.vue";
import StatsOverlay from "../components/StatsOverlay.vue";
import { PREFS_KEY, parsePrefs, type OverlayPrefs } from "../statsOverlay";
import WarningNote from "../components/WarningNote.vue";

// The environment, full screen. The portal brokers the connection; the picture
// and input go straight between this browser and the node.

const route = useRoute();
const id = computed(() => String(route.params.id));
const queryClient = useQueryClient();
const env = useQuery({ queryKey: ["environment", id], queryFn: () => api.environment(id.value), refetchInterval: 5000 });

const video = ref<HTMLVideoElement | null>(null);
const state = ref<PlayerState>("idle");
const problem = ref<string | null>(null);

const wtSupported = supportsWebTransport();
// PyroWave (the LAN tier) joins once WebGPU says it can decode it.
const codecs = ref<Codec[]>(supportedCodecs());
const PYROWAVE: Codec[] = ["pyrowave444", "pyrowave420"];
if (wtSupported) {
  void supportsPyroWave().then((ok) => {
    if (ok) codecs.value = [...codecs.value, ...PYROWAVE];
  });
}
/** The codecs the environment's device encodes, once known (null: any). */
const deviceCodecs = ref<string[] | null>(null);
/** What the codec menu offers: what this browser decodes and the device encodes. */
const availableCodecs = computed(() =>
  deviceCodecs.value ? codecs.value.filter((c) => deviceCodecs.value!.includes(c)) : codecs.value,
);
// PyroWave 4:4:4 is the LAN quality mode: near-lossless text, but ~580 Mbit/s
// and slower to the screen than the hardware codecs on 1 GbE (spike S6).
const CODEC_LABEL: Partial<Record<Codec, string>> = {
  pyrowave444: "LAN quality (PyroWave 4:4:4)",
  pyrowave420: "PyroWave 4:2:0",
};
const CODEC_KEY = "cha.player.codec";
const saved = (() => {
  try {
    return localStorage.getItem(CODEC_KEY) as Codec | null;
  } catch {
    return null;
  }
})();
// A saved PyroWave choice holds if this browser can do WebTransport (WebGPU is
// checked when connecting).
const codec = ref<Codec>(
  saved && (codecs.value.includes(saved) || (wtSupported && PYROWAVE.includes(saved)))
    ? saved
    : (codecs.value[0] ?? "h264"),
);

// WebTransport where the browser has it (Chromium), else WebRTC.
type TransportChoice = "auto" | Transport;
const TRANSPORT_KEY = "cha.player.transport";
const transportChoice = ref<TransportChoice>(
  (() => {
    try {
      const saved = localStorage.getItem(TRANSPORT_KEY) as TransportChoice | null;
      return saved && ["auto", "webrtc", "webtransport"].includes(saved) ? saved : "auto";
    } catch {
      return "auto";
    }
  })(),
);
/** The transport the connection ended up on. */
const transport = ref<Transport | null>(null);
function setTransport(t: TransportChoice) {
  transportChoice.value = t;
  try {
    localStorage.setItem(TRANSPORT_KEY, t);
  } catch {
    // Private mode: just this session.
  }
  retries = 0;
  void connect();
}

const SOUND_KEY = "cha.player.muted";
const muted = ref(
  (() => {
    try {
      return localStorage.getItem(SOUND_KEY) === "1";
    } catch {
      return false;
    }
  })(),
);
const VOLUME_KEY = "cha.player.volume";
/** Sound's level on this page, 0..100. */
const volume = ref(
  (() => {
    try {
      const v = Number(localStorage.getItem(VOLUME_KEY) ?? "100");
      return Number.isFinite(v) ? Math.min(100, Math.max(0, v)) : 100;
    } catch {
      return 100;
    }
  })(),
);
/** The browser holds sound back until a click or key press. */
const audioBlocked = ref(false);
/** Whether this page has the controls, and how many sessions watch (P2.6). */
const hasControl = ref(true);
const viewers = ref(1);
/** A short note when an app's copy reaches (or waits for) this device's clipboard. */
const clipboardNote = ref<string | null>(null);
let clipboardNoteTimer: ReturnType<typeof setTimeout> | undefined;
function noteClipboard(written: boolean) {
  clipboardNote.value = written ? "Copied to this device" : "Copied in the environment: click the picture to copy it here";
  clearTimeout(clipboardNoteTimer);
  clipboardNoteTimer = setTimeout(() => (clipboardNote.value = null), written ? 1500 : 4000);
}
/**
 * The frame rate the environment encodes at (the stream's, whoever set it), and
 * whether this page is changing it. An app whose display has a fixed size
 * (gamescope's) can't change refresh while it runs: no choice then.
 */
const fps = ref<number | null>(null);
const fpsSwitching = ref(false);
const fixedSize = ref(false);
async function setFps(rate: FrameRate, select: HTMLSelectElement) {
  if (!player || fpsSwitching.value) return;
  fpsSwitching.value = true;
  problem.value = null;
  try {
    await player.setFps(rate);
  } catch (err) {
    problem.value = err instanceof Error ? err.message : String(err);
  } finally {
    fpsSwitching.value = false;
    // Show what runs, if the streamer refused.
    select.value = String(fps.value ?? rate);
  }
}
/** The controllers this page sends, and the menu to add one (WebHID needs a click). */
const controllers = ref<ManagedController[]>([]);
/** Mouse capture after the browser let go (Esc): clicks recapture, and a hint says so. */
const capture = ref<CaptureView>({ state: "idle", recapture: false, hint: false });
function onCaptureButton() {
  if (capture.value.recapture) player?.turnOffMouseCapture();
  else void player?.lockPointer();
}
const controllerMenu = ref(false);
const controllerNote = ref<string | null>(null);
const hidReason = hidUnavailableReason();
async function connectController() {
  controllerNote.value = null;
  try {
    const added = await player?.connectHidController();
    const failed = player?.controllers?.webhid?.lastError;
    controllerNote.value = failed ?? (added ? null : "No controller was added.");
  } catch (err) {
    controllerNote.value = err instanceof Error ? err.message : String(err);
  }
}
function closeControllerMenu() {
  controllerMenu.value = false;
  controllerNote.value = null;
}
/** What the app's long setup is doing (a first-run download), over the black picture. */
const setup = ref<SetupStatus | null>(null);
const amount = (n: number) => n.toLocaleString(undefined, { maximumFractionDigits: n < 10 ? 1 : 0 });
/** "123 of 496 MB", or "123 MB" when the total isn't known. */
const setupDetail = computed(() => {
  const s = setup.value;
  if (s?.done === undefined) return null;
  const unit = s.unit ? ` ${s.unit}` : "";
  return s.total ? `${amount(s.done)} of ${amount(s.total)}${unit}` : `${amount(s.done)}${unit}`;
});
/** How far along, or null while that isn't known (an indeterminate bar). */
const setupPercent = computed(() => {
  const s = setup.value;
  return s?.total ? Math.min(100, Math.max(0, ((s.done ?? 0) / s.total) * 100)) : null;
});
function toggleSound() {
  muted.value = !muted.value;
  try {
    localStorage.setItem(SOUND_KEY, muted.value ? "1" : "0");
  } catch {
    // Private mode: just this session.
  }
  player?.setMuted(muted.value);
}
function onVolume(e: Event) {
  volume.value = Number((e.target as HTMLInputElement).value);
  try {
    localStorage.setItem(VOLUME_KEY, String(volume.value));
  } catch {
    // Private mode: just this session.
  }
  player?.setVolume(volume.value / 100);
  // Turning it up is asking for sound.
  if (muted.value && volume.value > 0) toggleSound();
}
function onSoundButton() {
  // Blocked: this click is the gesture the browser waits for.
  if (audioBlocked.value && !muted.value) player?.setMuted(false);
  else toggleSound();
}

let player: Player | null = null;
let retries = 0;
let retryTimer: ReturnType<typeof setTimeout> | undefined;
let leaving = false;
/** The latest connect() call: an older one still awaiting gives way. */
let attempt = 0;

async function connect() {
  clearTimeout(retryTimer);
  player?.close();
  if (!video.value) return;
  problem.value = null;
  setup.value = null;
  fps.value = null;
  const mine = ++attempt;
  // Fresh TURN credentials each time; a portal without TURN returns none.
  const iceServers = await api
    .iceServers()
    .then((r) => r.iceServers)
    .catch(() => []);
  // Whether its display has a fixed size (Steam's), which the picture then
  // keeps, and which codecs its device encodes (a CPU one only H.264).
  const { fixed, offered } = await Promise.all([
    queryClient.fetchQuery({ queryKey: ["environment", id], queryFn: () => api.environment(id.value), staleTime: 5000 }),
    queryClient.fetchQuery({ queryKey: ["catalog"], queryFn: api.catalog, staleTime: 60_000 }),
  ])
    .then(([e, templates]) => ({
      fixed: !!templates.find((t) => t.id === e.templateId)?.fixedSize,
      offered: e.codecs ?? null,
    }))
    .catch(() => ({ fixed: false, offered: null }));
  if (leaving || mine !== attempt || !video.value) return;
  fixedSize.value = fixed;
  deviceCodecs.value = offered;
  // A choice its device can't encode falls back to the first it can.
  if (offered && !offered.includes(codec.value)) {
    codec.value = availableCodecs.value[0] ?? (offered[0] as Codec | undefined) ?? "h264";
  }
  const p = new Player({
    video: video.value,
    fixedSize: fixed,
    codec: codec.value,
    iceServers,
    transport: transportChoice.value,
    webTransport: async (c) => {
      const r = await api.connect(id.value, { codec: c, transport: "webtransport" });
      return { urls: r.urls ?? [], certHash: r.certHash ?? "" };
    },
    onTransport: (t) => {
      if (player === p) transport.value = t;
    },
    muted: muted.value,
    volume: volume.value / 100,
    onAudioBlocked: (blocked) => {
      if (player === p) audioBlocked.value = blocked;
    },
    onClipboard: (_text, written) => {
      if (player === p) noteClipboard(written);
    },
    onFloor: (control, count) => {
      if (player !== p) return;
      hasControl.value = control;
      viewers.value = count;
    },
    onStatus: (status) => {
      if (player === p) setup.value = status;
    },
    onFps: (rate) => {
      if (player === p) fps.value = rate;
    },
    onControllers: (list) => {
      if (player === p) controllers.value = list;
    },
    onMouseCapture: (view) => {
      if (player === p) capture.value = view;
    },
    signal: async (offer, c) => (await api.connect(id.value, { codec: c, offer })).answer!,
    onState: (s, detail) => {
      if (player !== p) return;
      state.value = s;
      if (s === "connected") retries = 0;
      if (detail) problem.value = detail;
      // The status comes with the connection; the next one brings it again.
      if (s === "disconnected" || s === "failed") setup.value = null;
      // A dropped connection (Wi-Fi blip, node restart) comes back on its own.
      if ((s === "disconnected" || s === "failed") && !leaving && retries < 3) {
        retries++;
        retryTimer = setTimeout(connect, 1000 * retries);
      }
    },
  });
  player = p;
  try {
    await p.connect();
  } catch (err) {
    problem.value = err instanceof ApiError ? err.message : err instanceof Error ? err.message : String(err);
  }
}

async function setCodec(c: Codec) {
  codec.value = c;
  try {
    localStorage.setItem(CODEC_KEY, c);
  } catch {
    // Private mode: just this session.
  }
  retries = 0;
  if (!player || state.value !== "connected") return connect();
  // Over WebTransport the session switches in place; otherwise it reconnects.
  problem.value = null;
  try {
    await player.switchCodec(c);
  } catch (err) {
    problem.value = err instanceof Error ? err.message : String(err);
  }
}

// ---- Toolbar: collapses into a thin bar at the top; hover or click it back ----

const hover = ref(false);
/** Open: shown in full. Closed, it's a thin bar at the top centre. */
const expanded = ref(true);
let hideTimer: ReturnType<typeof setTimeout> | undefined;
function collapseSoon() {
  clearTimeout(hideTimer);
  hideTimer = setTimeout(() => (expanded.value = false), 1200);
}
function expand() {
  clearTimeout(hideTimer);
  expanded.value = true;
}
function onPointerMove(e: PointerEvent) {
  if (document.pointerLockElement || !expanded.value || hover.value) return;
  // Near the top it stays; away from it, it folds up shortly.
  if (e.clientY < 72) clearTimeout(hideTimer);
  else collapseSoon();
}
/** Fold the toolbar now, without waiting for the pointer to leave. */
function hideToolbar() {
  clearTimeout(hideTimer);
  hover.value = false;
  controllerMenu.value = false;
  expanded.value = false;
  (document.activeElement as HTMLElement | null)?.blur();
  video.value?.focus();
}
const toolbar = computed(() => state.value !== "connected" || expanded.value || hover.value || controllerMenu.value);

const fullscreen = ref(false);
async function toggleFullscreen() {
  if (document.fullscreenElement) {
    await document.exitFullscreen();
    return;
  }
  await document.documentElement.requestFullscreen({ navigationUI: "hide" });
  // Chromium: Esc, Cmd/Win and friends reach the environment; hold Esc to leave.
  await (navigator as Navigator & { keyboard?: { lock(): Promise<void> } }).keyboard?.lock().catch(() => {});
  video.value?.focus();
}
const onFullscreen = () => (fullscreen.value = !!document.fullscreenElement);

// ---- Stats and the probe ----

const overlay = ref<OverlayPrefs>(loadPrefs());
function loadPrefs(): OverlayPrefs {
  try {
    return parsePrefs(localStorage.getItem(PREFS_KEY));
  } catch {
    return parsePrefs(null);
  }
}
watch(overlay, (p) => {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(p));
  } catch {
    // private window or blocked storage: the panel just starts at its defaults next time
  }
});
const stats = ref<StatsSnapshot | null>(null);
// The health grade is judged from the last few snapshots, so the timer reads them (one getStats a
// second, cheap) whether or not the panel is open, and the toolbar's letter stays current. A hidden
// page's stats are throttled and say nothing about the stream, so they are not kept.
const statsHistory: StatsSnapshot[] = [];
const health = shallowRef<HealthAssessment>(assessHealth([], { visible: true }));
const statsTimer = setInterval(async () => {
  if (!player) return;
  const visible = document.visibilityState === "visible";
  if (!visible) {
    statsHistory.length = 0;
    health.value = assessHealth([], { visible });
    return;
  }
  const snapshot = await player.readStats();
  stats.value = snapshot;
  if (!snapshot) return;
  statsHistory.push(snapshot);
  if (statsHistory.length > HEALTH_WINDOW) statsHistory.shift();
  health.value = assessHealth(statsHistory, { visible });
  if (recording) {
    recording.snapshots.push(snapshot);
    recording.health.push(health.value);
  }
}, 1000);

// ---- Record 30 s: a measurement for the exit pass ----

const RECORD_S = 30;
let recording: {
  startedAt: number;
  snapshots: StatsSnapshot[];
  health: HealthAssessment[];
  stopLatencies: () => number[];
  timer: ReturnType<typeof setInterval>;
} | null = null;
/** Seconds left while recording, else null. */
const recordingLeft = ref<number | null>(null);
const recordingResult = ref<RecordingSummary | null>(null);

function startRecording() {
  if (!player || recording) return;
  const started = Date.now();
  recordingResult.value = null;
  recordingLeft.value = RECORD_S;
  recording = {
    startedAt: started,
    snapshots: [],
    health: [],
    stopLatencies: player.collectLatencies(),
    timer: setInterval(() => {
      // Wall clock, so a hidden tab (no snapshots then) doesn't stretch the run.
      const left = Math.max(0, RECORD_S - Math.round((Date.now() - started) / 1000));
      recordingLeft.value = left;
      if (left === 0) finishRecording();
    }, 250),
  };
}

function finishRecording() {
  const r = recording;
  if (!r) return;
  recording = null;
  clearInterval(r.timer);
  recordingLeft.value = null;
  recordingResult.value = summarizeRecording(r.snapshots, r.health, {
    userAgent: navigator.userAgent,
    transport: transport.value,
    environment: id.value,
    app: env.data.value?.templateName ?? null,
    startedAt: r.startedAt,
    durationS: RECORD_S,
    latencySamples: r.stopLatencies(),
  });
}

/** Abandons a run that is still going (the page is leaving); nothing is summarised. */
function cancelRecording() {
  if (!recording) return;
  clearInterval(recording.timer);
  recording.stopLatencies();
  recording = null;
  recordingLeft.value = null;
}
const GRADE_TEXT: Record<string, string> = { A: "text-accent", B: "text-accent", C: "text-warn", D: "text-warn", F: "text-danger" };
const gradeText = (grade: string | null) => (grade ? GRADE_TEXT[grade] : "text-ink-3");


const probing = ref(false);
const probe = ref<ProbeResult | null>(null);
async function runProbe() {
  if (!player) return;
  probing.value = true;
  probe.value = null;
  try {
    probe.value = await player.runProbe(25);
  } finally {
    probing.value = false;
  }
}

onMounted(() => {
  document.addEventListener("fullscreenchange", onFullscreen);
  void connect();
});

onBeforeUnmount(() => {
  leaving = true;
  clearTimeout(retryTimer);
  clearTimeout(hideTimer);
  clearInterval(statsTimer);
  cancelRecording();
  document.removeEventListener("fullscreenchange", onFullscreen);
  player?.close();
  if (document.fullscreenElement) void document.exitFullscreen();
});

const STATUS: Record<PlayerState, string> = {
  idle: "Starting…",
  connecting: "Connecting…",
  connected: "",
  disconnected: "Reconnecting…",
  failed: "Couldn't connect",
};
</script>

<template>
  <div class="fixed inset-0 bg-black select-none" @pointermove="onPointerMove">
    <video ref="video" class="absolute inset-0 size-full object-contain outline-none" autoplay muted playsinline />

    <!-- After Esc released the mouse: a click on the picture captures it again -->
    <div
      v-if="capture.hint"
      class="pointer-events-none absolute top-1/2 left-1/2 z-20 -translate-x-1/2 -translate-y-1/2 rounded-full border border-line bg-panel/80 px-4 py-2 text-sm shadow backdrop-blur"
      role="status"
    >
      Click to capture the mouse
    </div>

    <!-- The toolbar, folded: a thin bar to hover or click -->
    <button
      type="button"
      class="absolute top-0 left-1/2 z-30 -translate-x-1/2 px-6 pt-1.5 pb-3 transition-opacity duration-200"
      :class="toolbar ? 'pointer-events-none opacity-0' : 'opacity-100'"
      :tabindex="toolbar ? -1 : 0"
      aria-label="Show the toolbar"
      title="Show the toolbar"
      @pointerenter="expand"
      @click="expand"
      @focus="expand"
    >
      <span class="block h-1.5 w-24 rounded-full border border-line bg-panel/80 shadow backdrop-blur" />
    </button>

    <!-- Toolbar -->
    <div
      class="absolute top-3 left-1/2 z-30 flex origin-top -translate-x-1/2 items-center gap-1 rounded-xl border border-line bg-panel/90 p-1.5 whitespace-nowrap shadow-lg backdrop-blur transition duration-200"
      :class="toolbar ? 'opacity-100' : 'pointer-events-none -translate-y-3 scale-y-50 opacity-0'"
      :inert="!toolbar"
      @pointerenter="hover = true"
      @pointerleave="
        hover = false;
        collapseSoon();
      "
      @focusin="expand"
    >
      <RouterLink to="/" class="btn-ghost border-0 px-3 py-1.5 text-xs" title="Back to the dashboard (it keeps running)">← Back</RouterLink>
      <span class="max-w-48 truncate px-2 text-sm font-medium">{{ env.data.value?.templateName ?? "…" }}</span>
      <select
        class="rounded-lg border border-line bg-canvas px-2 py-1 text-xs text-ink-2"
        :value="codec"
        title="Video codec"
        @change="setCodec(($event.target as HTMLSelectElement).value as Codec)"
      >
        <option v-for="c in availableCodecs" :key="c" :value="c">{{ CODEC_LABEL[c] ?? c.toUpperCase() }}</option>
      </select>
      <select
        v-if="!fixedSize && fps !== null"
        :disabled="fpsSwitching || !hasControl || state !== 'connected'"
        class="rounded-lg border border-line bg-canvas px-2 py-1 text-xs text-ink-2"
        :value="fps"
        :title="hasControl ? 'Frame rate' : 'Only the session with the controls changes the frame rate'"
        aria-label="Frame rate"
        @change="setFps(Number(($event.target as HTMLSelectElement).value) as FrameRate, $event.target as HTMLSelectElement)"
      >
        <option v-for="rate in FRAME_RATES" :key="rate" :value="rate">{{ rate }} fps</option>
      </select>
      <select
        v-if="wtSupported"
        :disabled="isPyroWave(codec)"
        class="rounded-lg border border-line bg-canvas px-2 py-1 text-xs text-ink-2"
        :value="transportChoice"
        :title="transport ? `Connected over ${transport === 'webtransport' ? 'WebTransport' : 'WebRTC'}` : 'Transport'"
        @change="setTransport(($event.target as HTMLSelectElement).value as TransportChoice)"
      >
        <option value="auto">Auto{{ transport ? ` (${transport === "webtransport" ? "WT" : "RTC"})` : "" }}</option>
        <option value="webtransport">WebTransport</option>
        <option value="webrtc">WebRTC</option>
      </select>
      <span v-if="viewers > 1" class="px-2 text-xs text-ink-2" :title="`${viewers} sessions are watching this environment`">
        {{ viewers }} watching
      </span>
      <button
        v-if="!hasControl"
        class="btn-ghost border-0 px-3 py-1.5 text-xs text-accent"
        title="Someone else has the keyboard and mouse; take them"
        @click="player?.takeControl()"
      >
        Viewing · Take control
      </button>
      <button
        class="btn-ghost border-0 px-3 py-1.5 text-xs"
        :class="capture.recapture && 'text-accent'"
        :aria-pressed="capture.recapture"
        :title="
          capture.recapture
            ? 'Clicking the picture captures the mouse; click here to turn that off'
            : 'Raw mouse for games; Esc releases it (hold Esc in full screen on Chrome)'
        "
        @click="onCaptureButton"
      >
        {{ capture.recapture ? "Mouse capture on" : "Capture mouse" }}
      </button>
      <button
        class="btn-ghost border-0 px-3 py-1.5 text-xs"
        :class="audioBlocked && !muted && 'text-accent'"
        :title="audioBlocked && !muted ? 'Click to let the browser play sound' : 'Sound on or off'"
        @click="onSoundButton"
      >
        {{ muted ? "Sound off" : audioBlocked ? "Enable sound" : "Sound on" }}
      </button>
      <input
        type="range"
        min="0"
        max="100"
        step="5"
        class="w-20 accent-accent"
        :value="muted ? 0 : volume"
        :aria-valuetext="muted ? 'Sound off' : `${volume}%`"
        :title="muted ? 'Sound off' : `Volume ${volume}%`"
        aria-label="Volume"
        @input="onVolume"
      />
      <div class="relative" @keydown.esc="closeControllerMenu">
        <button
          class="btn-ghost border-0 px-3 py-1.5 text-xs"
          :class="controllers.length > 0 && 'text-accent'"
          aria-haspopup="true"
          :aria-expanded="controllerMenu"
          aria-controls="controller-menu"
          :title="controllers.length ? `${controllers.length} controller(s) sending input` : 'No controller yet'"
          @click="controllerMenu = !controllerMenu"
        >
          Controllers{{ controllers.length ? ` · ${controllers.length}` : "" }}
        </button>
        <div
          v-if="controllerMenu"
          id="controller-menu"
          class="absolute top-full right-0 z-20 mt-2 w-64 rounded-xl border border-line bg-panel p-3 text-left text-xs whitespace-normal shadow-lg"
        >
          <ul v-if="controllers.length" class="mb-3 space-y-1">
            <li v-for="c in controllers" :key="c.id" class="flex justify-between gap-2">
              <span class="truncate">{{ c.info.name }}</span>
              <span class="shrink-0 text-ink-3">{{ c.slot === null ? "no slot" : `slot ${c.slot + 1}` }}</span>
            </li>
          </ul>
          <p v-else class="mb-3 text-ink-2">Press a button on a controller. If nothing shows, connect it below.</p>
          <div class="flex flex-wrap items-center gap-2">
            <button
              class="btn-ghost px-3 py-1 text-xs"
              :disabled="!!hidReason || state !== 'connected'"
              :aria-describedby="hidReason ? 'controller-reason' : undefined"
              @click="connectController"
            >
              Connect a controller…
            </button>
            <RouterLink to="/controllers" class="text-accent hover:underline">Controllers page</RouterLink>
          </div>
          <p v-if="hidReason" id="controller-reason" class="mt-2 text-ink-3">{{ hidReason }}</p>
          <p v-if="controllerNote" role="status" class="mt-2 text-warn">{{ controllerNote }}</p>
        </div>
      </div>
      <button class="btn-ghost border-0 px-3 py-1.5 text-xs" @click="toggleFullscreen">
        {{ fullscreen ? "Exit full screen" : "Full screen" }}
      </button>
      <button class="btn-ghost border-0 px-3 py-1.5 text-xs" :class="overlay.open && 'text-accent'" :aria-pressed="overlay.open" @click="overlay = { ...overlay, open: !overlay.open }">
        Stats
        <span v-if="health.grade" class="ml-1 font-mono font-semibold" :class="gradeText(health.grade)" :title="`Stream health: ${health.summary}`">{{ health.grade }}</span>
      </button>
      <button
        v-if="env.data.value?.templateId === 'test-pattern'"
        class="btn-ghost border-0 px-3 py-1.5 text-xs"
        :disabled="probing || state !== 'connected'"
        title="25 synthetic clicks; times click → flash on screen"
        @click="runProbe"
      >
        {{ probing ? "Probing…" : "Probe" }}
      </button>
      <!-- Floats on the toolbar's lower edge, in the middle -->
      <button
        type="button"
        class="absolute top-full left-1/2 grid h-5 w-9 -translate-x-1/2 -translate-y-1/2 place-items-center rounded-full border border-line bg-panel text-ink-2 shadow transition hover:text-ink focus-visible:outline-2 focus-visible:outline-accent disabled:cursor-not-allowed disabled:opacity-50"
        :disabled="state !== 'connected'"
        aria-label="Hide the toolbar"
        title="Hide the toolbar (hover the thin bar at the top to bring it back)"
        @click="hideToolbar"
      >
        <svg viewBox="0 0 16 16" class="size-3.5" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M4 10l4-4 4 4" />
        </svg>
      </button>
    </div>

    <RecordingDialog :summary="recordingResult" @close="recordingResult = null" />

    <!-- Stats: under the toolbar (z-30) -->
    <StatsOverlay
      v-if="state === 'connected' || overlay.open"
      v-model="overlay"
      :stats="stats"
      :health="health"
      :codec="codec"
      :transport="transport"
      :connected="state === 'connected'"
      :recording-left="recordingLeft"
      :probe="probe"
      :toolbar-open="toolbar"
      @record="startRecording"
    />

    <!-- What the node noticed about this environment, which stays until it's gone -->
    <div
      v-if="state === 'connected' && env.data.value?.warning"
      class="pointer-events-none absolute top-16 left-1/2 z-10 w-xl max-w-[calc(100%-2rem)] -translate-x-1/2 rounded-lg bg-panel/90 backdrop-blur"
    >
      <WarningNote :message="env.data.value.warning" />
    </div>

    <div
      v-if="clipboardNote"
      class="pointer-events-none absolute bottom-4 left-1/2 -translate-x-1/2 rounded-lg border border-line bg-panel/90 px-3 py-1.5 text-sm backdrop-blur"
    >
      {{ clipboardNote }}
    </div>

    <!-- Setup status: the app's long first-run setup, while the picture is black -->
    <div v-if="setup && state === 'connected'" class="pointer-events-none absolute inset-0 grid place-items-center">
      <div role="status" class="w-80 max-w-[calc(100%-2rem)] rounded-xl border border-line bg-panel/90 px-6 py-5 text-center backdrop-blur">
        <p class="font-medium">{{ setup.label }}</p>
        <p v-if="setupDetail" class="mt-1 text-sm text-ink-2 tabular-nums">{{ setupDetail }}</p>
        <div
          class="mt-4 h-1.5 overflow-hidden rounded-full bg-canvas"
          role="progressbar"
          :aria-label="setup.label"
          aria-valuemin="0"
          aria-valuemax="100"
          :aria-valuenow="setupPercent === null ? undefined : Math.round(setupPercent)"
        >
          <div
            v-if="setupPercent !== null"
            class="h-full rounded-full bg-accent transition-[width] duration-300 ease-out"
            :style="{ width: `${setupPercent}%` }"
          />
          <div v-else class="h-full w-2/5 animate-indeterminate rounded-full bg-accent motion-reduce:w-full motion-reduce:animate-none motion-reduce:opacity-40" />
        </div>
      </div>
    </div>

    <!-- Status -->
    <div
      v-if="state !== 'connected'"
      class="absolute inset-0 grid place-items-center"
    >
      <div class="max-w-md rounded-xl border border-line bg-panel/90 px-6 py-5 text-center backdrop-blur">
        <p class="font-medium">{{ STATUS[state] }}</p>
        <p v-if="problem" class="mt-2 text-sm text-danger">{{ problem }}</p>
        <p v-if="env.data.value && env.data.value.state !== 'running'" class="mt-2 text-sm text-ink-2">
          The environment is {{ env.data.value.state }}<template v-if="env.data.value.detail">: {{ env.data.value.detail }}</template>.
        </p>
        <EnvironmentLog v-if="env.data.value?.log" :log="env.data.value.log" class="mt-3" />
        <button v-if="state === 'failed' || problem" class="btn-primary mt-4" @click="(retries = 0), connect()">Reconnect</button>
      </div>
    </div>
  </div>
</template>
