<script setup lang="ts">
import { Player, supportedCodecs, type PlayerState } from "@cha/player";
import { Eye, Gamepad2, Keyboard } from "lucide-vue-next";
import { computed, onBeforeUnmount, onMounted, ref, useTemplateRef } from "vue";
import { useRoute } from "vue-router";

import { api, type ShareInfo } from "../api";
import BrandMark from "../components/BrandMark.vue";
import FormError from "../components/FormError.vue";
import { backoffDelay } from "../reconnect";
import {
  controlPrompt,
  guestCanRetry,
  guestCodec,
  guestProblem,
  guestTransports,
  invitation,
  joinLabel,
  joinNote,
  seatLine,
} from "../shares";

// A share link's page (ADRs 0014 and 0015, /s/:token): no sign-in. It names the app, who invited
// the guest and in what role, and "Join" plays through @cha/player. A player sends only gamepads
// (no keyboard, mouse, pointer lock, clipboard or resize); a viewer sends nothing; a controller
// sends everything while it holds the controls, which it takes (when allowed) or is handed by the
// owner. A dropped stream reconnects on its own. An internet link (ADR 0022) tries WebRTC (it works
// when the guest can reach the node, or through the owner's TURN), then media over a WebSocket
// through the portal; the others try WebTransport first.
const route = useRoute();
const token = computed(() => String(route.params.token));
const video = useTemplateRef<HTMLVideoElement>("video");

const info = ref<ShareInfo | null>(null);
/** Why the link can't be used (shown instead of the invitation), or null. */
const dead = ref<string | null>(null);
const loading = ref(true);
const joined = ref(false);
const state = ref<PlayerState>("idle");
const problem = ref<string | null>(null);
/** "You are player 2", once the streamer says which pad is ours. */
const seat = ref<number | null>(null);
/** A controller: whether it holds the controls, and whether it could take them now. */
const hasControl = ref(false);
const canTake = ref(false);

let player: Player | null = null;
let attempt = 0;
let retries = 0;
let retryTimer: ReturnType<typeof setTimeout> | undefined;
let leaving = false;

onMounted(async () => {
  try {
    info.value = await api.shareInfo(token.value);
  } catch (err) {
    dead.value = guestProblem(err);
  } finally {
    loading.value = false;
  }
});

const role = computed(() => info.value?.role ?? "player");
const playerNumber = computed(() => (seat.value ?? info.value?.slot ?? 0) + 1);
const prompt = computed(() => controlPrompt(hasControl.value, canTake.value, info.value?.owner ?? "the owner"));

function stopForGood(err: unknown) {
  clearTimeout(retryTimer);
  player?.close();
  player = null;
  state.value = "failed";
  problem.value = guestProblem(err);
}

function scheduleRetry() {
  clearTimeout(retryTimer);
  retries++;
  retryTimer = setTimeout(() => void connect(), backoffDelay(retries));
}

async function connect() {
  clearTimeout(retryTimer);
  player?.close();
  const el = video.value;
  if (!el || leaving) return;
  problem.value = null;
  const mine = ++attempt;
  // ICE servers (the owner's TURN, if any); without them only direct paths. Never blocks joining.
  const iceServers = await api.shareIce(token.value).then((r) => r.iceServers, () => undefined);
  if (mine !== attempt || leaving) return;
  const p = new Player({
    video: el,
    codec: guestCodec(supportedCodecs(), info.value?.codecs ?? null),
    input: role.value === "viewer" ? "none" : role.value === "controller" ? "all" : "pads",
    // A guest never resizes the owner's screen.
    fixedSize: true,
    iceServers,
    transports: guestTransports(info.value?.wan ?? false),
    webTransport: async (codec) => {
      const r = await api.shareConnect(token.value, { codec, transport: "webtransport" });
      return { urls: r.urls ?? [], certHash: r.certHash ?? "" };
    },
    webSocket: async (codec) => {
      const r = await api.shareConnect(token.value, { codec, transport: "websocket" });
      return { urls: r.urls ?? [] };
    },
    signal: async (offer, codec) => (await api.shareConnect(token.value, { codec, offer })).answer!,
    onFloor: (control, _viewers, who, take) => {
      if (player !== p) return;
      if (who !== undefined) seat.value = who;
      hasControl.value = control;
      canTake.value = take === true;
    },
    onState: (s, detail) => {
      if (player !== p) return;
      state.value = s;
      if (s === "connected") retries = 0;
      if (detail) problem.value = detail;
      if ((s === "disconnected" || s === "failed") && !leaving) scheduleRetry();
    },
  });
  player = p;
  try {
    await p.connect();
  } catch (err) {
    if (player !== p || mine !== attempt || leaving) return;
    if (guestCanRetry(err)) {
      state.value = "disconnected";
      problem.value = guestProblem(err);
      scheduleRetry();
    } else {
      stopForGood(err);
    }
  }
}

async function join() {
  joined.value = true;
  retries = 0;
  // The <video> exists once `joined` renders.
  await new Promise((r) => requestAnimationFrame(r));
  await connect();
}

onBeforeUnmount(() => {
  leaving = true;
  clearTimeout(retryTimer);
  player?.close();
  player = null;
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
  <main v-if="!joined" class="grid min-h-dvh place-items-center bg-canvas px-4 py-10 text-ink">
    <div class="card w-full max-w-sm space-y-5 p-6 text-center">
      <BrandMark class="mx-auto size-12" />
      <p v-if="loading" class="text-sm text-ink-2" role="status">Checking the link…</p>
      <template v-else-if="info">
        <div class="mx-auto grid size-12 place-items-center rounded-xl bg-accent-soft text-accent">
          <Eye v-if="info.role === 'viewer'" class="size-6" aria-hidden="true" />
          <Keyboard v-else-if="info.role === 'controller'" class="size-6" aria-hidden="true" />
          <Gamepad2 v-else class="size-6" aria-hidden="true" />
        </div>
        <div class="space-y-1">
          <h1 class="text-lg font-semibold tracking-tight">{{ info.app }}</h1>
          <p class="text-sm text-ink-2">{{ invitation(info.role, info.owner, info.slot) }}</p>
        </div>
        <button type="button" class="btn-primary w-full" @click="join">{{ joinLabel(info.role, info.slot) }}</button>
        <p class="text-xs text-ink-3">{{ joinNote(info.role) }}</p>
      </template>
      <template v-else>
        <h1 class="text-lg font-semibold tracking-tight">Can't join</h1>
        <FormError :message="dead" />
      </template>
    </div>
  </main>

  <div v-else class="fixed inset-0 bg-black select-none">
    <video ref="video" class="absolute inset-0 size-full object-contain outline-none" autoplay muted playsinline />
    <div v-if="state !== 'connected'" class="absolute inset-0 grid place-items-center">
      <div class="max-w-md rounded-xl border border-line bg-panel/90 px-6 py-5 text-center text-ink backdrop-blur transparency-reduced:bg-panel transparency-reduced:backdrop-blur-none">
        <p class="font-medium">{{ STATUS[state] }}</p>
        <p v-if="problem" class="mt-2 text-sm text-danger" role="status">{{ problem }}</p>
      </div>
    </div>
    <div v-else-if="role === 'controller'" class="absolute top-3 left-1/2 flex -translate-x-1/2 items-center gap-2" role="status">
      <button
        v-if="prompt.kind === 'take'"
        type="button"
        class="btn-primary min-h-9 px-4"
        @click="player?.takeControl()"
      >
        {{ prompt.text }}
      </button>
      <p
        v-else
        class="pointer-events-none rounded-full border border-line bg-panel/80 px-3 py-1 text-xs text-ink backdrop-blur transparency-reduced:bg-panel transparency-reduced:backdrop-blur-none"
      >
        {{ prompt.text }}
      </p>
    </div>
    <p
      v-else
      class="pointer-events-none absolute top-3 left-1/2 -translate-x-1/2 rounded-full border border-line bg-panel/80 px-3 py-1 text-xs text-ink backdrop-blur transparency-reduced:bg-panel transparency-reduced:backdrop-blur-none"
      role="status"
    >
      {{ seatLine(role, playerNumber) }}
    </p>
  </div>
</template>
