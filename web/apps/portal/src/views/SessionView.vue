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
  type OverlayLevel,
  type OverlayState,
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
import { computed, onBeforeUnmount, onMounted, ref, shallowRef, useTemplateRef, watch } from "vue";
import { useRoute, useRouter } from "vue-router";

import { api } from "../api";
import { backoffDelay, classifyConnectError } from "../reconnect";
import EnvironmentLog from "../components/EnvironmentLog.vue";
import GpuBadge from "../components/GpuBadge.vue";
import PowerOffDialog from "../components/PowerOffDialog.vue";
import RecordingDialog from "../components/RecordingDialog.vue";
import StatsOverlay from "../components/StatsOverlay.vue";
import { useSession } from "../stores/session";
import { setForcedDark } from "../themes/runtime";
import { loadToolbarPrefs, saveToolbarPrefs, toolbarKey, type ToolbarPrefs } from "../toolbarPrefs";
import { PREFS_KEY, parsePrefs, type OverlayPrefs } from "../statsOverlay";
import WarningNote from "../components/WarningNote.vue";

// The environment, full screen. The portal brokers the connection; the picture
// and input go straight between this browser and the node.

const route = useRoute();
const id = computed(() => String(route.params.id));
const queryClient = useQueryClient();
const router = useRouter();
const session = useSession();

// ---- Remembered toolbar settings: per user and app type, applied once the app type is known ----

let profileKey: string | null = null;
/** Saved frame rate and overlay level, applied when the stream reports them (they need the controls). */
let pendingFps: number | undefined;
let pendingOverlay: number | undefined;
const remember = (patch: ToolbarPrefs) => {
  if (profileKey) saveToolbarPrefs(profileKey, patch);
};
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
  remember({ transport: t });
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
    remember({ fps: rate });
  } catch (err) {
    problem.value = err instanceof Error ? err.message : String(err);
  } finally {
    fpsSwitching.value = false;
    // Show what runs, if the streamer refused.
    select.value = String(fps.value ?? rate);
  }
}
/**
 * The app's performance overlay (Steam's MangoHud): the level now, whoever set
 * it, or null when the app has none (then the page offers nothing).
 */
// A saved frame rate or overlay level is applied once, when the stream first reports its own
// (only the session with the controls may change them; on a fixed-size app there is no choice).
watch(fps, (rate) => {
  const want = pendingFps;
  if (rate === null || want === undefined) return;
  pendingFps = undefined;
  if (want !== rate && player && hasControl.value && !fixedSize.value) void player.setFps(want as FrameRate).catch(() => {});
});
const OVERLAY_LABEL: Record<OverlayLevel, string> = { 0: "Off", 1: "FPS", 2: "Bar", 3: "Detailed", 4: "Full" };
const perfOverlay = ref<OverlayState | null>(null);
const overlaySwitching = ref(false);
watch(perfOverlay, (level) => {
  const want = pendingOverlay;
  if (level === null || want === undefined) return;
  pendingOverlay = undefined;
  if (want !== level && player && hasControl.value) void player.setOverlay(want as OverlayLevel).catch(() => {});
});
async function setOverlay(level: OverlayLevel, select: HTMLSelectElement) {
  if (!player || overlaySwitching.value) return;
  overlaySwitching.value = true;
  problem.value = null;
  try {
    await player.setOverlay(level);
    remember({ overlay: level });
  } catch (err) {
    problem.value = err instanceof Error ? err.message : String(err);
  } finally {
    overlaySwitching.value = false;
    // Show what is set, if the streamer refused.
    select.value = String(perfOverlay.value ?? level);
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
/** The toolbar's mouse switch: off, the mouse is not sent (the keyboard and controllers still are). It outlasts a reconnect. */
const mouseOn = ref(true);
function toggleMouse() {
  mouseOn.value = !mouseOn.value;
  player?.setMouseEnabled(mouseOn.value);
  remember({ mouse: mouseOn.value });
}
// ---- Power off: a short countdown the user can cancel, then the environment is stopped ----

const powerOff = ref(false);
const powerOffError = ref<string | null>(null);
function askPowerOff() {
  menu.value = null;
  powerOffError.value = null;
  powerOff.value = true;
}
async function confirmPowerOff() {
  const wasLeaving = leaving;
  leaving = true; // the stream ending is expected: no reconnect
  try {
    await api.stopEnvironment(id.value);
    void queryClient.invalidateQueries({ queryKey: ["environments"] });
    await router.push("/");
  } catch (err) {
    leaving = wasLeaving;
    powerOffError.value = err instanceof Error ? err.message : String(err);
  }
}

/** The toolbar's open dropdown, if any. */
type MenuName = "stream" | "sound" | "controllers";
const menu = ref<MenuName | null>(null);
const toggleMenu = (name: MenuName) => {
  menu.value = menu.value === name ? null : name;
  if (menu.value !== "controllers") controllerNote.value = null;
};
const ICON_BTN = "btn-ghost relative grid size-8 place-items-center border-0 p-0";
const MENU_BOX = "absolute top-full z-20 mt-2 w-64 rounded-xl border border-line bg-panel p-3 text-left text-xs whitespace-normal shadow-lg";
const SELECT = "w-full rounded-lg border border-line-strong bg-canvas px-2 py-1 text-xs text-ink-2 disabled:opacity-50";
/** A click anywhere outside an open dropdown closes it. */
function onDocPointerDown(e: PointerEvent) {
  if (menu.value && !(e.target as Element | null)?.closest("[data-menu]")) closeMenu();
}
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
function closeMenu() {
  menu.value = null;
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
  remember({ muted: muted.value });
  player?.setMuted(muted.value);
}
function onVolume(e: Event) {
  volume.value = Number((e.target as HTMLInputElement).value);
  try {
    localStorage.setItem(VOLUME_KEY, String(volume.value));
  } catch {
    // Private mode: just this session.
  }
  remember({ volume: volume.value });
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

// ---- Staying connected: reconnects back off, wake on the network or the page coming back ----

/** An automatic reconnect is scheduled (so the Reconnect button isn't needed). */
const retryPending = ref(false);
/** Times the stream came back on its own after dropping. */
const reconnects = ref(0);
/** The last drop was followed by an automatic reconnect that hasn't connected yet. */
let recovering = false;
/** The environment isn't running: reconnect when the page's environment query says it is. */
let waitingForRunning = false;
/** A connect failed in a way retrying won't fix; only the user (or the network/page coming back) tries again. */
let halted = false;

function cancelRetry() {
  clearTimeout(retryTimer);
  retryTimer = undefined;
  retryPending.value = false;
}

/** Schedules the next automatic reconnect with backoff; offline, it waits for the `online` event instead. */
function scheduleRetry() {
  cancelRetry();
  if (leaving || halted || waitingForRunning) return;
  if (navigator.onLine === false) {
    problem.value = OFFLINE_NOTE;
    return;
  }
  retries++;
  retryPending.value = true;
  retryTimer = setTimeout(() => {
    retryPending.value = false;
    recovering = true;
    void connect();
  }, backoffDelay(retries));
}

const OFFLINE_NOTE = "Offline, waiting for the network…";

/** The network or the page came back: try now rather than at the end of the backoff. */
function reconnectNow() {
  if (leaving || (state.value !== "disconnected" && state.value !== "failed")) return;
  retries = 0;
  halted = false;
  waitingForRunning = false;
  cancelRetry();
  if (navigator.onLine === false) {
    problem.value = OFFLINE_NOTE;
    return;
  }
  recovering = true;
  void connect();
}
const onOnline = () => reconnectNow();
const onPageShow = () => reconnectNow();
const onVisible = () => {
  if (document.visibilityState === "visible") reconnectNow();
};
const onOffline = () => {
  if (state.value !== "connected") problem.value = OFFLINE_NOTE;
};

// Reconnect on its own when an environment that stopped is running again.
watch(
  () => env.dataUpdatedAt.value,
  () => {
    if (waitingForRunning && env.data.value?.state === "running") {
      waitingForRunning = false;
      retries = 0;
      recovering = true;
      void connect();
    }
  },
);

/** What a failed connect() means: back off and retry, wait for the environment, sign in again, or stop and say why. */
function onConnectError(err: unknown) {
  const failure = classifyConnectError(err);
  const message = err instanceof Error ? err.message : String(err);
  switch (failure.kind) {
    case "retry":
      problem.value = message;
      if (!retryPending.value) scheduleRetry(); // the failed state usually scheduled it already
      break;
    case "wait-running":
      cancelRetry();
      waitingForRunning = true;
      problem.value = `${failure.reason}. Reconnecting when it is running again.`;
      // One that was stopped (idle shutoff, power off) or failed never runs again: say so.
      void env.refetch().then((r) => {
        const st = r.data?.state;
        if (st === "destroyed" || st === "failed") {
          waitingForRunning = false;
          halted = true;
          problem.value = null;
        }
      });
      break;
    case "login":
      cancelRetry();
      halted = true;
      session.user = null;
      void router.replace({ name: "login", query: { next: route.fullPath } });
      break;
    case "stop":
      cancelRetry();
      halted = true;
      problem.value = failure.reason;
      break;
  }
}
/** The latest connect() call: an older one still awaiting gives way. */
let attempt = 0;

/** Load this user's saved settings for the app type, and set the toolbar to them. */
function applyProfile(key: string) {
  profileKey = key;
  const saved = loadToolbarPrefs(key);
  if (saved.codec && (codecs.value.includes(saved.codec as Codec) || (wtSupported && PYROWAVE.includes(saved.codec as Codec)))) {
    codec.value = saved.codec as Codec;
  }
  if (saved.transport) transportChoice.value = saved.transport;
  if (saved.muted !== undefined) muted.value = saved.muted;
  if (saved.volume !== undefined) volume.value = saved.volume;
  if (saved.mouse !== undefined) mouseOn.value = saved.mouse;
  pendingFps = saved.fps;
  pendingOverlay = saved.overlay;
}

async function connect() {
  cancelRetry();
  halted = false;
  waitingForRunning = false;
  player?.close();
  if (!video.value) return;
  problem.value = null;
  setup.value = null;
  fps.value = null;
  perfOverlay.value = null;
  const mine = ++attempt;
  // Fresh TURN credentials each time; a portal without TURN returns none.
  const iceServers = await api
    .iceServers()
    .then((r) => r.iceServers)
    .catch(() => []);
  // Whether its display has a fixed size (Steam's), which the picture then
  // keeps, and which codecs its device encodes (a CPU one only H.264).
  const { fixed, offered, templateId } = await Promise.all([
    queryClient.fetchQuery({ queryKey: ["environment", id], queryFn: () => api.environment(id.value), staleTime: 5000 }),
    queryClient.fetchQuery({ queryKey: ["catalog"], queryFn: api.catalog, staleTime: 60_000 }),
  ])
    .then(([e, templates]) => ({
      fixed: !!templates.find((t) => t.id === e.templateId)?.fixedSize,
      offered: e.codecs ?? null,
      templateId: e.templateId,
    }))
    .catch(() => ({ fixed: false, offered: null, templateId: null }));
  if (leaving || mine !== attempt || !video.value) return;
  if (!profileKey && templateId && session.user?.id) applyProfile(toolbarKey(session.user.id, templateId));
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
    onOverlay: (level) => {
      if (player === p) perfOverlay.value = level;
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
      if (s === "connected") {
        retries = 0;
        cancelRetry(); // a drop that healed itself must not tear the stream down a second later
        halted = false;
        waitingForRunning = false;
        if (recovering) reconnects.value++;
        recovering = false;
        if (!mouseOn.value) p.setMouseEnabled(false);
      }
      if (detail) problem.value = detail;
      // The status comes with the connection; the next one brings it again.
      if (s === "disconnected" || s === "failed") setup.value = null;
      // A dropped connection (Wi-Fi blip, node restart) comes back on its own.
      if ((s === "disconnected" || s === "failed") && !leaving) scheduleRetry();
    },
  });
  player = p;
  try {
    await p.connect();
  } catch (err) {
    if (player === p && !leaving) onConnectError(err);
  }
}

async function setCodec(c: Codec) {
  codec.value = c;
  try {
    localStorage.setItem(CODEC_KEY, c);
  } catch {
    // Private mode: just this session.
  }
  remember({ codec: c });
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
  menu.value = null;
  expanded.value = false;
  (document.activeElement as HTMLElement | null)?.blur();
  video.value?.focus();
}
// Wrapped onto more rows on a narrow screen, the toolbar is taller; the stats panel sits below
// whatever height it has.
const toolbarEl = useTemplateRef<HTMLElement>("toolbarEl");
const toolbarHeight = ref(0);
let toolbarWatch: ResizeObserver | null = null;
watch(toolbarEl, (el) => {
  toolbarWatch?.disconnect();
  toolbarWatch = null;
  if (!el) return;
  toolbarHeight.value = el.offsetHeight;
  toolbarWatch = new ResizeObserver(() => (toolbarHeight.value = el.offsetHeight));
  toolbarWatch.observe(el);
});
onBeforeUnmount(() => toolbarWatch?.disconnect());
const toolbar = computed(() => state.value !== "connected" || expanded.value || hover.value || menu.value !== null);

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
const GRADE_TEXT: Record<string, string> = { A: "text-ok", B: "text-ok", C: "text-warn", D: "text-warn", F: "text-danger" };
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
  // The stream view stays dark and theme-neutral, whatever appearance the user picked.
  setForcedDark(true);
  document.addEventListener("fullscreenchange", onFullscreen);
  document.addEventListener("pointerdown", onDocPointerDown);
  window.addEventListener("online", onOnline);
  window.addEventListener("offline", onOffline);
  window.addEventListener("pageshow", onPageShow);
  document.addEventListener("visibilitychange", onVisible);
  void connect();
});

onBeforeUnmount(() => {
  setForcedDark(false);
  leaving = true;
  cancelRetry();
  window.removeEventListener("online", onOnline);
  window.removeEventListener("offline", onOffline);
  window.removeEventListener("pageshow", onPageShow);
  document.removeEventListener("visibilitychange", onVisible);
  clearTimeout(hideTimer);
  clearInterval(statsTimer);
  cancelRecording();
  document.removeEventListener("fullscreenchange", onFullscreen);
  document.removeEventListener("pointerdown", onDocPointerDown);
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
      class="pointer-events-none absolute top-1/2 left-1/2 z-20 -translate-x-1/2 -translate-y-1/2 rounded-full border border-line bg-panel/80 px-4 py-2 text-sm shadow backdrop-blur transparency-reduced:bg-panel transparency-reduced:backdrop-blur-none"
      role="status"
    >
      Click for exclusive input
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
      <span class="block h-1.5 w-24 rounded-full border border-line bg-panel/80 shadow backdrop-blur transparency-reduced:bg-panel transparency-reduced:backdrop-blur-none" />
    </button>

    <!-- Toolbar -->
    <div
      ref="toolbarEl"
      class="absolute inset-x-2 top-3 z-30 mx-auto flex w-max max-w-[calc(100%-1rem)] origin-top flex-wrap items-center justify-center gap-1 rounded-xl border border-line bg-panel/90 p-1.5 shadow-lg backdrop-blur transparency-reduced:bg-panel transparency-reduced:backdrop-blur-none transition duration-200"
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
      <button
        :class="[ICON_BTN, 'hover:text-danger']"
        aria-label="Power off"
        title="Power off this app (a 5 second countdown, which you can cancel)"
        @click="askPowerOff"
      >
        <svg viewBox="0 0 16 16" class="size-4" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M8 1.5v6" />
          <path d="M4.6 3.9a5.5 5.5 0 1 0 6.8 0" />
        </svg>
      </button>
      <span class="max-w-48 truncate px-2 text-sm font-medium">{{ env.data.value?.templateName ?? "…" }}</span>
      <GpuBadge v-if="env.data.value?.device" :kind="env.data.value.device.kind" :name="env.data.value.device.name" tense="is" />
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

      <!-- Stream settings -->
      <div class="relative" data-menu @keydown.esc="closeMenu">
        <button
          :class="[ICON_BTN, menu === 'stream' && 'bg-line/60 text-accent']"
          aria-haspopup="true"
          :aria-expanded="menu === 'stream'"
          aria-controls="stream-menu"
          aria-label="Stream settings"
          title="Stream settings: codec, frame rate, transport"
          @click="toggleMenu('stream')"
        >
          <svg viewBox="0 0 16 16" class="size-4" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M2 4h12M2 8h12M2 12h12" /><circle cx="5" cy="4" r="1.6" fill="var(--color-panel)" /><circle cx="10.5" cy="8" r="1.6" fill="var(--color-panel)" /><circle cx="6.5" cy="12" r="1.6" fill="var(--color-panel)" /></svg>
        </button>
        <div v-if="menu === 'stream'" id="stream-menu" :class="[MENU_BOX, 'left-0 space-y-2']">
          <label class="block">
            <span class="mb-0.5 block text-ink-2">Video codec</span>
            <select :class="SELECT" :value="codec" @change="setCodec(($event.target as HTMLSelectElement).value as Codec)">
              <option v-for="c in availableCodecs" :key="c" :value="c">{{ CODEC_LABEL[c] ?? c.toUpperCase() }}</option>
            </select>
          </label>
          <label v-if="!fixedSize && fps !== null" class="block">
            <span class="mb-0.5 block text-ink-2">Frame rate</span>
            <select
              :class="SELECT"
              :disabled="fpsSwitching || !hasControl || state !== 'connected'"
              :value="fps"
              :title="hasControl ? 'Frame rate' : 'Only the session with the controls changes the frame rate'"
              @change="setFps(Number(($event.target as HTMLSelectElement).value) as FrameRate, $event.target as HTMLSelectElement)"
            >
              <option v-for="rate in FRAME_RATES" :key="rate" :value="rate">{{ rate }} fps</option>
            </select>
          </label>
          <label v-if="perfOverlay !== null" class="block">
            <span class="mb-0.5 block text-ink-2">Steam Gamescope Overlay</span>
            <select
              :class="SELECT"
              :disabled="overlaySwitching || !hasControl || state !== 'connected'"
              :value="perfOverlay"
              :title="hasControl ? 'Steam Gamescope Overlay' : 'Only the session with the controls changes the overlay'"
              @change="setOverlay(Number(($event.target as HTMLSelectElement).value) as OverlayLevel, $event.target as HTMLSelectElement)"
            >
              <option v-if="perfOverlay === 'custom'" value="custom" disabled>Custom</option>
              <option v-for="(label, level) in OVERLAY_LABEL" :key="level" :value="level">{{ label }}</option>
            </select>
          </label>
          <label v-if="wtSupported" class="block">
            <span class="mb-0.5 block text-ink-2">Transport</span>
            <select
              :class="SELECT"
              :disabled="isPyroWave(codec)"
              :value="transportChoice"
              :title="transport ? `Connected over ${transport === 'webtransport' ? 'WebTransport' : 'WebRTC'}` : 'Transport'"
              @change="setTransport(($event.target as HTMLSelectElement).value as TransportChoice)"
            >
              <option value="auto">Auto{{ transport ? ` (${transport === "webtransport" ? "WT" : "RTC"})` : "" }}</option>
              <option value="webtransport">WebTransport</option>
              <option value="webrtc">WebRTC</option>
            </select>
          </label>
          <button
            v-if="env.data.value?.templateId === 'test-pattern'"
            class="btn-ghost w-full px-3 py-1 text-xs"
            :disabled="probing || state !== 'connected'"
            title="25 synthetic clicks; times click → flash on screen"
            @click="runProbe"
          >
            {{ probing ? "Probing…" : "Run the click probe" }}
          </button>
        </div>
      </div>

      <!-- Input -->
      <button
        :class="[ICON_BTN, capture.recapture && 'text-accent']"
        :disabled="!mouseOn"
        :aria-pressed="capture.recapture"
        :aria-label="capture.recapture ? 'Exclusive input on' : 'Exclusive input'"
        :title="
          !mouseOn
            ? 'Turn the mouse on to use exclusive input'
            : capture.recapture
              ? 'Exclusive input on: clicking the picture captures the mouse; click here to turn that off'
              : 'Exclusive input: the mouse is captured for games; Esc releases it (hold Esc in full screen on Chrome)'
        "
        @click="onCaptureButton"
      >
        <svg viewBox="0 0 16 16" class="size-4" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <circle cx="8" cy="8" r="3.5" />
          <path d="M8 1.5v3M8 11.5v3M1.5 8h3M11.5 8h3" />
          <circle v-if="capture.recapture" cx="8" cy="8" r="0.8" fill="currentColor" />
        </svg>
      </button>
      <button
        :class="[ICON_BTN, !mouseOn && 'text-warn']"
        :aria-pressed="!mouseOn"
        :aria-label="mouseOn ? 'Turn the mouse off' : 'Turn the mouse on'"
        :title="mouseOn ? 'Mouse on: click to stop sending the mouse (the keyboard still works)' : 'Mouse off: click to send the mouse again'"
        @click="toggleMouse"
      >
        <svg viewBox="0 0 16 16" class="size-4" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <rect x="4.5" y="1.5" width="7" height="13" rx="3.5" />
          <path d="M8 4.5v2.5" />
          <path v-if="!mouseOn" d="M2 2l12 12" />
        </svg>
      </button>

      <!-- Sound -->
      <div class="relative" data-menu @keydown.esc="closeMenu">
        <button
          :class="[ICON_BTN, menu === 'sound' && 'bg-line/60', audioBlocked && !muted ? 'text-accent' : muted && 'text-ink-2']"
          aria-haspopup="true"
          :aria-expanded="menu === 'sound'"
          aria-controls="sound-menu"
          :aria-label="muted ? 'Sound off' : audioBlocked ? 'Enable sound' : 'Sound on'"
          :title="audioBlocked && !muted ? 'The browser is blocking sound: open this and click Enable sound' : muted ? 'Sound off' : `Sound on, volume ${volume}%`"
          @click="toggleMenu('sound')"
        >
          <svg viewBox="0 0 16 16" class="size-4" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M2.5 6h2.5l3-2.5v9L5 10H2.5z" />
            <path v-if="muted" d="M11 6l3 4M14 6l-3 4" />
            <path v-else d="M10.5 5.5a3.5 3.5 0 0 1 0 5M12.3 3.8a6 6 0 0 1 0 8.4" />
          </svg>
        </button>
        <div v-if="menu === 'sound'" id="sound-menu" :class="[MENU_BOX, 'left-0 space-y-2']">
          <button
            class="btn-ghost w-full px-3 py-1 text-xs"
            :class="audioBlocked && !muted && 'text-accent'"
            :title="audioBlocked && !muted ? 'Click to let the browser play sound' : 'Sound on or off'"
            @click="onSoundButton"
          >
            {{ muted ? "Turn sound on" : audioBlocked ? "Enable sound" : "Turn sound off" }}
          </button>
          <label class="flex items-center gap-2">
            <span class="text-ink-2">Volume</span>
            <input
              type="range"
              min="0"
              max="100"
              step="5"
              class="min-w-0 flex-1 accent-accent"
              :value="muted ? 0 : volume"
              :aria-valuetext="muted ? 'Sound off' : `${volume}%`"
              @input="onVolume"
            />
            <span class="w-8 text-right">{{ muted ? 0 : volume }}%</span>
          </label>
          <button
            v-if="!muted"
            class="btn-ghost w-full px-3 py-1 text-xs"
            title="Sound went quiet? This rebuilds the page's sound decoder and playback without reconnecting"
            @click="player?.restartAudio()"
          >
            Restart sound
          </button>
        </div>
      </div>

      <!-- Controllers -->
      <div class="relative" data-menu @keydown.esc="closeMenu">
        <button
          :class="[ICON_BTN, controllers.length > 0 && 'text-accent', menu === 'controllers' && 'bg-line/60']"
          aria-haspopup="true"
          :aria-expanded="menu === 'controllers'"
          aria-controls="controller-menu"
          :aria-label="`Controllers${controllers.length ? `, ${controllers.length} sending input` : ''}`"
          :title="controllers.length ? `${controllers.length} controller(s) sending input` : 'No controller yet'"
          @click="toggleMenu('controllers')"
        >
          <svg viewBox="0 0 16 16" class="size-4" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <rect x="1.5" y="4.5" width="13" height="7.5" rx="3.5" />
            <path d="M5 6.7v3M3.5 8.2h3" />
            <circle cx="10.5" cy="7.3" r=".6" fill="currentColor" /><circle cx="12" cy="9.1" r=".6" fill="currentColor" />
          </svg>
          <span v-if="controllers.length" class="absolute -top-0.5 -right-0.5 grid min-w-3.5 place-items-center rounded-full bg-accent px-1 text-[9px] leading-3.5 font-semibold text-accent-ink">{{ controllers.length }}</span>
        </button>
        <div v-if="menu === 'controllers'" id="controller-menu" :class="[MENU_BOX, 'right-0 p-3']">
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

      <!-- View -->
      <button
        :class="ICON_BTN"
        :aria-label="fullscreen ? 'Exit full screen' : 'Full screen'"
        :title="fullscreen ? 'Exit full screen' : 'Full screen'"
        @click="toggleFullscreen"
      >
        <svg viewBox="0 0 16 16" class="size-4" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path v-if="fullscreen" d="M6 2v4H2M14 6h-4V2M10 14v-4h4M2 10h4v4" />
          <path v-else d="M2 6V2h4M10 2h4v4M14 10v4h-4M6 14H2v-4" />
        </svg>
      </button>
      <button
        :class="[ICON_BTN, 'w-auto gap-1 px-2', overlay.open && 'text-accent']"
        :aria-pressed="overlay.open"
        aria-label="Stats"
        :title="overlay.open ? 'Hide the stats panel' : 'Show the stats panel'"
        @click="overlay = { ...overlay, open: !overlay.open }"
      >
        <svg viewBox="0 0 16 16" class="size-4" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" aria-hidden="true">
          <path d="M3 13V8M8 13V3M13 13V6" />
        </svg>
        <span v-if="health.grade" class="font-mono text-xs font-semibold" :class="gradeText(health.grade)" :title="`Stream health: ${health.summary}`">{{ health.grade }}</span>
      </button>
      <!-- Floats on the toolbar's lower edge, in the middle -->
      <button
        type="button"
        class="absolute top-full left-1/2 grid h-5 w-9 -translate-x-1/2 -translate-y-1/2 place-items-center rounded-full border border-line bg-panel text-ink-2 shadow transition hover:text-ink focus-visible:outline-2 focus-visible:outline-focus disabled:cursor-not-allowed disabled:opacity-50"
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

    <PowerOffDialog
      :open="powerOff"
      :name="env.data.value?.templateName ?? 'the app'"
      :failed="powerOffError"
      @cancel="powerOff = false"
      @confirm="confirmPowerOff"
    />
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
      :reconnects="reconnects"
      :probe="probe"
      :toolbar-inset="toolbar ? toolbarHeight : 0"
      @record="startRecording"
    />

    <!-- What the node noticed about this environment, which stays until it's gone -->
    <div
      v-if="state === 'connected' && env.data.value?.warning"
      class="pointer-events-none absolute top-16 left-1/2 z-10 w-xl max-w-[calc(100%-2rem)] -translate-x-1/2 rounded-lg bg-panel/90 backdrop-blur transparency-reduced:bg-panel transparency-reduced:backdrop-blur-none"
    >
      <WarningNote :message="env.data.value.warning" />
    </div>

    <div
      v-if="clipboardNote"
      class="pointer-events-none absolute bottom-4 left-1/2 -translate-x-1/2 rounded-lg border border-line bg-panel/90 px-3 py-1.5 text-sm backdrop-blur transparency-reduced:bg-panel transparency-reduced:backdrop-blur-none"
    >
      {{ clipboardNote }}
    </div>

    <!-- Setup status: the app's long first-run setup, while the picture is black -->
    <div v-if="setup && state === 'connected'" class="pointer-events-none absolute inset-0 grid place-items-center">
      <div role="status" class="w-80 max-w-[calc(100%-2rem)] rounded-xl border border-line bg-panel/90 px-6 py-5 text-center backdrop-blur transparency-reduced:bg-panel transparency-reduced:backdrop-blur-none">
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
          <div v-else class="h-full w-2/5 animate-indeterminate rounded-full bg-accent" />
        </div>
      </div>
    </div>

    <!-- Status -->
    <div
      v-if="state !== 'connected'"
      class="absolute inset-0 grid place-items-center"
    >
      <div class="max-w-md rounded-xl border border-line bg-panel/90 px-6 py-5 text-center backdrop-blur transparency-reduced:bg-panel transparency-reduced:backdrop-blur-none">
        <p class="font-medium">{{ STATUS[state] }}</p>
        <p v-if="problem" class="mt-2 text-sm text-danger">{{ problem }}</p>
        <p v-if="env.data.value && env.data.value.state !== 'running'" class="mt-2 text-sm text-ink-2">
          The environment is {{ env.data.value.state }}<template v-if="env.data.value.detail">: {{ env.data.value.detail }}</template>.
        </p>
        <p v-if="env.data.value?.state === 'destroyed' || env.data.value?.state === 'failed'" class="mt-2 text-sm text-ink-2">
          It won't come back on its own. Start it again from the dashboard.
        </p>
        <RouterLink
          v-if="env.data.value?.state === 'destroyed' || env.data.value?.state === 'failed'"
          to="/"
          class="btn-primary mt-4 inline-block"
        >
          Back to the dashboard
        </RouterLink>
        <EnvironmentLog v-if="env.data.value?.log" :log="env.data.value.log" class="mt-3" />
        <button
          v-if="env.data.value?.state !== 'destroyed' && (state === 'failed' || problem || (state === 'disconnected' && !retryPending))"
          class="btn-primary mt-4"
          @click="(retries = 0), connect()"
        >
          Reconnect
        </button>
      </div>
    </div>
  </div>
</template>
