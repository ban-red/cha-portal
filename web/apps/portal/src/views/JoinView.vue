<script setup lang="ts">
import { Player, supportedCodecs, supportsWebTransport, type PlayerState } from "@cha/player";
import { Gamepad2 } from "lucide-vue-next";
import { computed, onBeforeUnmount, onMounted, ref, useTemplateRef } from "vue";
import { useRoute } from "vue-router";

import { api, type ShareInfo } from "../api";
import BrandMark from "../components/BrandMark.vue";
import FormError from "../components/FormError.vue";
import { backoffDelay } from "../reconnect";
import { guestCanRetry, guestCodec, guestProblem, playerLabel } from "../shares";

// A share link's page (ADR 0014, /s/:token): no sign-in. It names the app and who invited
// the guest, and "Join" plays through @cha/player with only the gamepads sent (no keyboard,
// mouse, pointer lock, clipboard or resize). A dropped stream reconnects on its own.
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

const playerNumber = computed(() => (seat.value ?? info.value?.slot ?? 0) + 1);

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
  const p = new Player({
    video: el,
    codec: guestCodec(supportedCodecs(), info.value?.codecs ?? null),
    input: "pads",
    transport: supportsWebTransport() ? "auto" : "webrtc",
    webTransport: async (codec) => {
      const r = await api.shareConnect(token.value, { codec, transport: "webtransport" });
      return { urls: r.urls ?? [], certHash: r.certHash ?? "" };
    },
    signal: async (offer, codec) => (await api.shareConnect(token.value, { codec, offer })).answer!,
    onFloor: (_control, _viewers, who) => {
      if (player === p && who !== undefined) seat.value = who;
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
          <Gamepad2 class="size-6" aria-hidden="true" />
        </div>
        <div class="space-y-1">
          <h1 class="text-lg font-semibold tracking-tight">{{ info.app }}</h1>
          <p class="text-sm text-ink-2">{{ info.owner }} invited you to play as {{ playerLabel(info.slot) }}.</p>
        </div>
        <button type="button" class="btn-primary w-full" @click="join">Join as {{ playerLabel(info.slot) }}</button>
        <p class="text-xs text-ink-3">Connect a gamepad, then press a button on it. Only the gamepad is shared.</p>
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
    <p
      v-else
      class="pointer-events-none absolute top-3 left-1/2 -translate-x-1/2 rounded-full border border-line bg-panel/80 px-3 py-1 text-xs text-ink backdrop-blur transparency-reduced:bg-panel transparency-reduced:backdrop-blur-none"
      role="status"
    >
      You are player {{ playerNumber }}. Press a button on your gamepad.
    </p>
  </div>
</template>
