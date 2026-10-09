<script setup lang="ts">
import {
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
  type Watcher,
  watcherLabel,
} from "@cha/player";
import { buildToolbar, pickText, TOOLBAR, type ToolbarState } from "@cha/ui-spec";
import { useQuery, useQueryClient } from "@tanstack/vue-query";
import { computed, onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";
import { useRoute, useRouter } from "vue-router";

import { api } from "../api";
import { backoffDelay, classifyConnectError } from "../reconnect";
import EnvironmentLog from "../components/EnvironmentLog.vue";
import PowerOffDialog from "../components/PowerOffDialog.vue";
import ShareDialog from "../components/ShareDialog.vue";
import RecordingDialog from "../components/RecordingDialog.vue";
import SessionToolbar from "../components/SessionToolbar.vue";
import StatsOverlay from "../components/StatsOverlay.vue";
import { useSession } from "../stores/session";
import { setForcedDark } from "../themes/runtime";
import { loadToolbarPrefs, saveToolbarPrefs, toolbarKey, type ToolbarPrefs } from "../toolbarPrefs";
import { PREFS_KEY, parsePrefs, type OverlayPrefs } from "../statsOverlay";
import WarningNote from "../components/WarningNote.vue";
import LaunchProgress from "../components/LaunchProgress.vue";

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
// and slower to the screen than the hardware codecs on 1 GbE (spike S6). The menu's labels for the
// codecs are toolbar.json's.
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

// WebTransport where the browser has it (Chromium), else WebRTC; WebSocket (media through the portal,
// ADR 0022) can be picked for testing.
type TransportChoice = "auto" | Transport;
const TRANSPORT_KEY = "cha.player.transport";
const transportChoice = ref<TransportChoice>(
  (() => {
    try {
      const saved = localStorage.getItem(TRANSPORT_KEY) as TransportChoice | null;
      return saved && ["auto", "webrtc", "webtransport", "websocket"].includes(saved) ? saved : "auto";
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
/** The other sessions, while this page has the controls (ADR 0015): who's watching, and who can be handed them. */
const watchers = ref<Watcher[]>([]);
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
/** Esc held with the mouse captured: the progress towards letting go, or null while no hint shows. */
const escProgress = ref<number | null>(null);
const releaseHint = pickText(TOOLBAR.release_hint, "web") ?? "";
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

const sharing = ref(false);
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
    webSocket: async (c) => {
      const r = await api.connect(id.value, { codec: c, transport: "websocket" });
      return { urls: r.urls ?? [] };
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
    onViewers: (list) => {
      if (player === p) watchers.value = list;
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
    onEscHold: (progress) => {
      if (player === p) escProgress.value = progress;
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
  hideTimer = setTimeout(() => (expanded.value = false), TOOLBAR.timing.fold_after_ms);
}
function expand() {
  clearTimeout(hideTimer);
  expanded.value = true;
}
function onPointerMove(e: PointerEvent) {
  if (document.pointerLockElement || !expanded.value || hover.value) return;
  // Near the top it stays; away from it, it folds up shortly.
  if (e.clientY < TOOLBAR.timing.near_top_px) clearTimeout(hideTimer);
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
// whatever height it has (the toolbar reports it).
const toolbarHeight = ref(0);

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

// ---- The toolbar: the spec's model from this page's state ----

/** What this browser's session can do: the portal brokers it, so everything but what the browser or the app lacks. */
const capabilities = computed(() =>
  TOOLBAR.reports.web.filter((cap) => {
    if (cap === "transport-switch") return wtSupported;
    if (cap === "probe") return env.data.value?.templateId === "test-pattern";
    if (cap === "gpu-badge") return !!env.data.value?.device;
    return true;
  }),
);

const toolbarState = computed<ToolbarState>(() => ({
  connected: state.value === "connected",
  expanded: expanded.value,
  hover: hover.value,
  menu_open: menu.value ?? "",
  countdown: null,
  title: env.data.value?.templateName ?? "",
  gpu_kind: env.data.value?.device?.kind ?? "",
  viewers: viewers.value,
  has_control: hasControl.value,
  watchers: watchers.value.map((w) => ({ id: String(w.id), label: watcherLabel(w), can_hand: w.role === "controller" })),
  codec: codec.value,
  codecs: availableCodecs.value,
  pyrowave: isPyroWave(codec.value),
  fps: fps.value,
  fixed_size: fixedSize.value,
  fps_switching: fpsSwitching.value,
  overlay: perfOverlay.value === null ? null : String(perfOverlay.value),
  overlay_switching: overlaySwitching.value,
  transport_choice: transportChoice.value,
  transport_via: transport.value ?? "",
  probing: probing.value,
  mouse_on: mouseOn.value,
  recapture: capture.value.recapture,
  muted: muted.value,
  volume: volume.value,
  audio_blocked: audioBlocked.value,
  controllers: controllers.value.map((c) => ({ name: c.info.name, slot: c.slot })),
  hid_reason: hidReason ?? "",
  note: controllerNote.value ?? "",
  fullscreen: fullscreen.value,
  stats_open: overlay.value.open,
  grade: health.value.grade ?? "",
  grade_summary: health.value.summary,
}));
const toolbarModel = computed(() => buildToolbar(TOOLBAR, toolbarState.value, capabilities.value, "web"));

function giveControl(watcherId: string) {
  player?.giveControl(Number(watcherId));
}

/** A control in the toolbar was clicked. */
function onControl(controlId: string) {
  switch (controlId) {
    case "power":
      askPowerOff();
      break;
    case "share":
      sharing.value = true;
      break;
    case "take-control":
      player?.takeControl();
      break;
    case "capture":
      onCaptureButton();
      break;
    case "mouse":
      toggleMouse();
      break;
    case "fullscreen":
      void toggleFullscreen();
      break;
    case "stats":
      overlay.value = { ...overlay.value, open: !overlay.value.open };
      break;
    case "stream":
    case "sound":
    case "controllers":
      toggleMenu(controlId);
      break;
  }
}

/** A row in an open menu was used. */
function onRow(menuId: string, rowId: string, value?: string | Event, el?: HTMLSelectElement) {
  const picked = typeof value === "string" ? value : "";
  switch (`${menuId}/${rowId}`) {
    case "stream/codec":
      void setCodec(picked as Codec);
      break;
    case "stream/fps":
      if (el) void setFps(Number(picked) as FrameRate, el);
      break;
    case "stream/overlay":
      if (el) void setOverlay(Number(picked) as OverlayLevel, el);
      break;
    case "stream/transport":
      setTransport(picked as TransportChoice);
      break;
    case "stream/probe":
      void runProbe();
      break;
    case "sound/sound-toggle":
      onSoundButton();
      break;
    case "sound/volume":
      if (value && typeof value !== "string") onVolume(value);
      break;
    case "sound/restart":
      player?.restartAudio();
      break;
    case "controllers/connect":
      void connectController();
      break;
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

    <!-- Esc held while the mouse is captured: keep holding to let go (the words and timing are toolbar.json's) -->
    <div
      v-if="escProgress !== null"
      class="pointer-events-none absolute top-20 left-1/2 z-20 w-72 max-w-[calc(100%-2rem)] -translate-x-1/2 rounded-xl border border-line bg-panel/90 px-4 py-2.5 text-center text-sm shadow-lg backdrop-blur transparency-reduced:bg-panel transparency-reduced:backdrop-blur-none"
      role="status"
    >
      {{ releaseHint }}
      <div class="mt-2 h-1 overflow-hidden rounded-full bg-line" role="progressbar" aria-valuemin="0" aria-valuemax="100" :aria-valuenow="Math.round(escProgress * 100)">
        <div class="h-full rounded-full bg-accent" :style="{ width: `${Math.round(escProgress * 100)}%` }" />
      </div>
    </div>

    <!-- The toolbar (and its folded bar): what it shows is toolbar.json's, built into `toolbarModel` -->
    <SessionToolbar
      :model="toolbarModel"
      :gpu="env.data.value?.device ? { kind: env.data.value.device.kind, name: env.data.value.device.name } : null"
      @control="onControl"
      @hand="giveControl"
      @row="onRow"
      @close-menu="closeMenu"
      @expand="expand"
      @pointer-enter="hover = true"
      @pointer-leave="
        hover = false;
        collapseSoon();
      "
      @hide="hideToolbar"
      @height="(px) => (toolbarHeight = px)"
    />

    <PowerOffDialog
      :open="powerOff"
      :name="env.data.value?.templateName ?? TOOLBAR.power_off.default_name"
      :failed="powerOffError"
      @cancel="powerOff = false"
      @confirm="confirmPowerOff"
    />
    <ShareDialog :open="sharing" :environment-id="id" :name="env.data.value?.templateName ?? 'the app'" @close="sharing = false" />
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
      :toolbar-inset="toolbarModel.visible ? toolbarHeight : 0"
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
        <LaunchProgress
          v-if="env.data.value?.state === 'starting' && env.data.value.progress"
          :env="env.data.value"
          class="mt-3 w-72 max-w-full"
        />
        <p v-else-if="env.data.value && env.data.value.state !== 'running'" class="mt-2 text-sm text-ink-2">
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
