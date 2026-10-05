<script setup lang="ts">
import {
  Player,
  supportedCodecs,
  supportsPyroWave,
  supportsWebTransport,
  isPyroWave,
  hidUnavailableReason,
  type Codec,
  type ManagedController,
  type PlayerState,
  type ProbeResult,
  type SetupStatus,
  type StatsSnapshot,
  type Transport,
} from "@cha/player";
import { useQuery, useQueryClient } from "@tanstack/vue-query";
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { useRoute } from "vue-router";

import { ApiError, api } from "../api";
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
/** The controllers this page sends, and the menu to add one (WebHID needs a click). */
const controllers = ref<ManagedController[]>([]);
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
  const mine = ++attempt;
  // Fresh TURN credentials each time; a portal without TURN returns none.
  const iceServers = await api
    .iceServers()
    .then((r) => r.iceServers)
    .catch(() => []);
  // Whether its display has a fixed size (Steam's), which the picture then keeps.
  const fixedSize = await Promise.all([
    queryClient.fetchQuery({ queryKey: ["environment", id], queryFn: () => api.environment(id.value), staleTime: 5000 }),
    queryClient.fetchQuery({ queryKey: ["catalog"], queryFn: api.catalog, staleTime: 60_000 }),
  ])
    .then(([e, templates]) => !!templates.find((t) => t.id === e.templateId)?.fixedSize)
    .catch(() => false);
  if (leaving || mine !== attempt || !video.value) return;
  const p = new Player({
    video: video.value,
    fixedSize,
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
    onControllers: (list) => {
      if (player === p) controllers.value = list;
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

// ---- Toolbar: shown near the top edge, or while not connected ----

const hover = ref(false);
const nearTop = ref(true);
let hideTimer: ReturnType<typeof setTimeout> | undefined;
function onPointerMove(e: PointerEvent) {
  if (document.pointerLockElement) return;
  const top = e.clientY < 72;
  if (top) {
    clearTimeout(hideTimer);
    nearTop.value = true;
  } else if (nearTop.value && !hover.value) {
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => (nearTop.value = false), 1200);
  }
}
const toolbar = computed(() => state.value !== "connected" || nearTop.value || hover.value);

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

const showStats = ref(false);
const stats = ref<StatsSnapshot | null>(null);
const statsTimer = setInterval(async () => {
  if (showStats.value && player) stats.value = await player.readStats();
}, 1000);
const fmt = (v: number | null, digits = 1, unit = "") => (v === null || Number.isNaN(v) ? "–" : `${v.toFixed(digits)}${unit}`);

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

    <!-- Toolbar -->
    <div
      class="absolute top-3 left-1/2 z-10 flex -translate-x-1/2 items-center gap-1 rounded-xl border border-line bg-panel/90 p-1.5 whitespace-nowrap shadow-lg backdrop-blur transition-opacity duration-200"
      :class="toolbar ? 'opacity-100' : 'pointer-events-none opacity-0'"
      @pointerenter="hover = true"
      @pointerleave="hover = false"
    >
      <RouterLink to="/" class="btn-ghost border-0 px-3 py-1.5 text-xs" title="Back to the dashboard (it keeps running)">← Back</RouterLink>
      <span class="max-w-48 truncate px-2 text-sm font-medium">{{ env.data.value?.templateName ?? "…" }}</span>
      <select
        class="rounded-lg border border-line bg-canvas px-2 py-1 text-xs text-ink-2"
        :value="codec"
        title="Video codec"
        @change="setCodec(($event.target as HTMLSelectElement).value as Codec)"
      >
        <option v-for="c in codecs" :key="c" :value="c">{{ CODEC_LABEL[c] ?? c.toUpperCase() }}</option>
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
      <button class="btn-ghost border-0 px-3 py-1.5 text-xs" title="Raw mouse for games; Esc releases it" @click="player?.lockPointer()">
        Capture mouse
      </button>
      <button
        class="btn-ghost border-0 px-3 py-1.5 text-xs"
        :class="audioBlocked && !muted && 'text-accent'"
        :title="audioBlocked && !muted ? 'Click to let the browser play sound' : 'Sound on or off'"
        @click="onSoundButton"
      >
        {{ muted ? "Sound off" : audioBlocked ? "Enable sound" : "Sound on" }}
      </button>
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
      <button class="btn-ghost border-0 px-3 py-1.5 text-xs" :class="showStats && 'text-accent'" @click="showStats = !showStats">
        Stats
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
    </div>

    <!-- Stats -->
    <div
      v-if="showStats"
      class="absolute top-3 left-3 z-10 rounded-lg border border-line bg-panel/90 px-3 py-2 font-mono text-[11px] leading-5 text-ink-2 backdrop-blur"
    >
      <div>{{ stats?.codec ?? codec.toUpperCase() }} · {{ stats?.width ?? "–" }}×{{ stats?.height ?? "–" }} · {{ fmt(stats?.fps ?? null, 0) }} fps</div>
      <div>{{ fmt(stats?.mbps ?? null, 1, " Mbit/s") }} · RTT {{ fmt(stats?.rttMs ?? null, 1, " ms") }}</div>
      <div class="text-ink">send → shown {{ fmt(stats?.latencyMs ?? null, 1, " ms") }}</div>
      <div>decode {{ fmt(stats?.decodeMs ?? null, 2, " ms") }} · jitter buf {{ fmt(stats?.jitterMs ?? null, 2, " ms") }}</div>
      <div>lost {{ stats?.packetsLost ?? 0 }} · dropped {{ stats?.framesDropped ?? 0 }} · audio buf {{ fmt(stats?.audioJitterMs ?? null, 0, " ms") }}</div>
      <div v-if="probe" class="mt-1 border-t border-line pt-1 text-accent">
        click → shown {{ fmt(probe.clickToPresentedMs.p50) }} / {{ fmt(probe.clickToPresentedMs.p95) }} ms
        ({{ probe.samples }}, {{ probe.missed }} missed)
        <template v-if="probe.audioSamples">
          <br />click → sound {{ fmt(probe.clickToAudioMs.p50) }} / {{ fmt(probe.clickToAudioMs.p95) }} ms · A/V
          {{ fmt(probe.avOffsetMs.p50) }} ms
        </template>
      </div>
    </div>

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
        <button v-if="state === 'failed' || problem" class="btn-primary mt-4" @click="(retries = 0), connect()">Reconnect</button>
      </div>
    </div>
  </div>
</template>
