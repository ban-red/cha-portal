<script setup lang="ts">
import {
  ControllerManager,
  EXTRA,
  type ControllerState,
  type ControllerType,
  type ManagedController,
  type RawReport,
} from "@cha/player";
import { onBeforeUnmount, onMounted, ref } from "vue";

import AppControllerKinds from "../components/AppControllerKinds.vue";
import ControllerDiagram from "../components/ControllerDiagram.vue";
import FormError from "../components/FormError.vue";
import ToggleSwitch from "../components/ToggleSwitch.vue";
import WarningNote from "../components/WarningNote.vue";

// What this browser sees of the controllers, and what it would send to an
// environment. It only watches: nothing here reaches a session.

const manager = new ControllerManager();
const controllers = ref<ManagedController[]>([]);
const hidReason = manager.hidUnavailable;

interface Live {
  state: ControllerState;
  raw: RawReport | null;
}
const live = ref<Record<string, Live>>({});
const showRaw = ref(false);
const connecting = ref(false);
const note = ref<string | null>(null);
const problem = ref<string | null>(null);

let frame = 0;
function loop() {
  const next: Record<string, Live> = {};
  for (const c of controllers.value) {
    const s = manager.state(c.id);
    next[c.id] = {
      state: {
        buttons: [...s.buttons],
        axes: [...s.axes],
        gyro: s.gyro && [...s.gyro],
        accel: s.accel && [...s.accel],
        touch: s.touch?.map((t) => ({ ...t })),
        battery: s.battery,
      },
      raw: showRaw.value ? manager.raw(c.id) : null,
    };
  }
  live.value = next;
  frame = requestAnimationFrame(loop);
}

let unsubscribe = () => {};
onMounted(() => {
  unsubscribe = manager.onChange((list) => {
    controllers.value = [...list];
    // Something arrived: whatever failed before is old news.
    problem.value = manager.webhid?.lastError ?? null;
  });
  manager.start();
  frame = requestAnimationFrame(loop);
});
onBeforeUnmount(() => {
  cancelAnimationFrame(frame);
  unsubscribe();
  manager.stop();
});

async function connect() {
  note.value = null;
  problem.value = null;
  connecting.value = true;
  try {
    const added = await manager.connectHid();
    problem.value = manager.webhid?.lastError ?? null;
    if (!added && !problem.value) note.value = "No controller was added.";
  } catch (err) {
    problem.value = err instanceof Error ? err.message : String(err);
  } finally {
    connecting.value = false;
  }
}

function rumble(c: ManagedController) {
  manager.rumbleController(c.id, 1, 0.6, 500);
}

// A DualSense's lightbar and adaptive triggers: a colour each press, and a
// resistance (the "feedback" effect, mode 0x21: the active-zones mask, then three bits of strength per
// zone, here all ten zones at 3 of 0..7; SDL itself only documents modes 0x01, 0x05 and 0x06, in
// test/testcontroller.c) for a few seconds, then mode 0x05, which SDL uses to clear one.
const LIGHTS: [number, number, number][] = [[255, 40, 40], [40, 255, 40], [60, 60, 255], [255, 255, 255]];
const lightStep: Record<string, number> = {};
const FEEDBACK = (() => {
  let packed = 0;
  for (let zone = 0; zone < 10; zone++) packed += 3 * 8 ** zone;
  return [0x21, 0xff, 0x03, packed & 0xff, (packed >>> 8) & 0xff, (packed >>> 16) & 0xff, (packed >>> 24) & 0xff, 0, 0, 0, 0];
})();
const triggerTimers = new Map<string, ReturnType<typeof setTimeout>>();
onBeforeUnmount(() => triggerTimers.forEach(clearTimeout));

function testLight(c: ManagedController) {
  const step = (lightStep[c.id] = ((lightStep[c.id] ?? -1) + 1) % LIGHTS.length);
  manager.pad(c.id)?.led?.(...LIGHTS[step]!);
}

function testTriggers(c: ManagedController) {
  const pad = manager.pad(c.id);
  clearTimeout(triggerTimers.get(c.id));
  for (const side of ["left", "right"] as const) pad?.trigger?.(side, FEEDBACK);
  triggerTimers.set(
    c.id,
    setTimeout(() => {
      for (const side of ["left", "right"] as const) manager.pad(c.id)?.trigger?.(side, [0x05, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }, 3000),
  );
}

const BACKEND: Record<string, string> = { gamepad: "Gamepad API", webhid: "WebHID" };
const TYPE: Record<string, string> = {
  xbox: "Xbox",
  playstation: "PlayStation",
  switch: "Nintendo",
  steam: "Steam",
  generic: "Generic",
};
const hex = (n: number | undefined) => (n === undefined ? "" : n.toString(16).padStart(4, "0"));
const hexBytes = (bytes: Uint8Array) =>
  Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join(" ") || "(empty)";

function capabilities(c: ManagedController): string {
  const caps = c.info.capabilities;
  const list = [
    caps.rumble && "rumble",
    caps.gyro && "gyro",
    caps.touchpad && "touchpad",
    caps.battery && "battery",
    caps.lightbar && "lightbar",
    caps.triggers && "adaptive triggers",
  ].filter(Boolean);
  return list.length ? list.join(", ") : "buttons and sticks";
}

const BUTTON_NAMES = [
  "South", "East", "West", "North", "Left shoulder", "Right shoulder", "Left trigger", "Right trigger",
  "Back", "Start", "Left stick", "Right stick", "D-pad up", "D-pad down", "D-pad left", "D-pad right", "Guide",
];
// What the buttons past the standard 17 are on each kind of controller.
const EXTRA_NAMES: Partial<Record<ControllerType, Record<number, string>>> = {
  steam: {
    [EXTRA.touchpadClick]: "Right trackpad click",
    [EXTRA.leftPadClick]: "Left trackpad click",
    [EXTRA.l4]: "Left grip",
    [EXTRA.r4]: "Right grip",
    [EXTRA.l5]: "L5",
    [EXTRA.r5]: "R5",
    [EXTRA.mute]: "Quick access",
  },
  playstation: {
    [EXTRA.touchpadClick]: "Touchpad click",
    [EXTRA.l4]: "Left paddle",
    [EXTRA.r4]: "Right paddle",
    [EXTRA.l5]: "Left function",
    [EXTRA.r5]: "Right function",
    [EXTRA.mute]: "Mute",
  },
};
/** Which buttons are held, in words (the diagram is only a picture). */
function pressed(s: ControllerState | undefined, type: ControllerType): string {
  if (!s) return "";
  const names = BUTTON_NAMES.filter((_, i) => (s.buttons[i] ?? 0) > 0.5);
  for (const [i, name] of Object.entries(EXTRA_NAMES[type] ?? {})) if ((s.buttons[Number(i)] ?? 0) > 0.5) names.push(name);
  return names.length ? names.join(", ") : "nothing";
}

interface PadArea {
  label: string;
  /** Wide, like a DualSense's touchpad; else square. */
  wide: boolean;
  clicked: boolean;
  dots: { x: number; y: number }[];
}
/** Where fingers are: a Steam Controller has two trackpads, a DualSense one touchpad for both fingers. */
function areas(c: ManagedController, s: ControllerState): PadArea[] {
  const touch = s.touch ?? [];
  const held = (i: number) => (s.buttons[i] ?? 0) > 0.5;
  if (c.info.type === "steam") {
    return [
      { label: "Left trackpad", wide: false, clicked: held(EXTRA.leftPadClick), dots: touch.filter((t) => t.id === 0) },
      { label: "Right trackpad", wide: false, clicked: held(EXTRA.touchpadClick), dots: touch.filter((t) => t.id === 1) },
    ];
  }
  return [{ label: "Touchpad", wide: true, clicked: held(EXTRA.touchpadClick), dots: touch }];
}
const pct = (v: number | undefined) => `${Math.round((v ?? 0) * 100)}%`;
const num = (v: number | undefined, digits = 2) => (v ?? 0).toFixed(digits);
const AXES = ["Left X", "Left Y", "Right X", "Right Y"];

type Raw<K extends RawReport["kind"]> = Extract<RawReport, { kind: K }>;
const rawGamepad = (id: string) => (live.value[id]?.raw?.kind === "gamepad" ? (live.value[id]!.raw as Raw<"gamepad">) : null);
const rawHid = (id: string) => (live.value[id]?.raw?.kind === "hid" ? (live.value[id]!.raw as Raw<"hid">) : null);
</script>

<template>
  <div class="max-w-3xl space-y-6">
    <div class="space-y-1 text-sm text-ink-2">
      <p>
        The controllers this browser can see, and what a session would send to an environment. A controller
        has to show up here for a game to see it.
      </p>
    </div>

    <div class="card flex flex-wrap items-center justify-between gap-x-6 gap-y-3 px-4 py-4 sm:px-5">
      <div class="min-w-0 text-sm">
        <p class="text-base font-medium">Connect a controller…</p>
        <p :id="hidReason ? 'hid-reason' : undefined" class="mt-0.5 text-xs text-ink-3">
          {{
            hidReason ??
            "For one the browser's Gamepad API doesn't show, such as a Steam Controller or some Bluetooth pads. Pick it in the browser's list."
          }}
        </p>
      </div>
      <div class="flex items-center gap-4">
        <div class="flex items-center gap-2 text-sm">
          <ToggleSwitch v-model="showRaw" aria-labelledby="raw-label" />
          <span id="raw-label">Raw view</span>
        </div>
        <button
          type="button"
          class="btn-primary shrink-0"
          :disabled="!!hidReason || connecting"
          :aria-describedby="hidReason ? 'hid-reason' : undefined"
          @click="connect"
        >
          {{ connecting ? "Choosing…" : "Connect a controller…" }}
        </button>
      </div>
    </div>

    <div aria-live="polite" class="space-y-2">
      <FormError polite :message="problem" />
      <p v-if="note" class="text-xs text-ink-2">{{ note }}</p>
    </div>

    <div v-if="!controllers.length" class="card space-y-3 px-6 py-8 text-sm">
      <p class="text-lg font-semibold tracking-tight">No controllers yet</p>
      <ul class="list-disc space-y-1.5 pl-5 text-ink-2">
        <li>Press a button on the controller with this page in front: browsers show a controller only after its first press.</li>
        <li>
          On a Mac, check that it's connected in System Settings → Game Controllers. If Chrome still doesn't see it,
          turn on "Increase controller compatibility" there for Chrome.
        </li>
        <li>
          8BitDo controllers have to be in their Mac or Bluetooth mode (the D or BT position of the mode switch, per
          8BitDo's manual for your model) to be seen at all.
        </li>
        <li>
          A Steam Controller isn't a gamepad until something claims it: use "Connect a controller…" and pick it.
          Quit Steam first if it's running, or it keeps the controller.
        </li>
        <li v-if="hidReason">{{ hidReason }}</li>
        <li v-else>
          This browser has WebHID: if the controller still doesn't show, use "Connect a controller…" above and look
          for it in the browser's list. If the list doesn't have it, the system isn't passing it to the browser.
        </li>
      </ul>
    </div>

    <ul v-else class="space-y-4">
      <li v-for="c in controllers" :key="c.id" class="card px-4 py-4 sm:px-5" :aria-labelledby="`${c.id}-name`">
        <div class="flex flex-wrap items-start justify-between gap-x-4 gap-y-2">
          <div class="min-w-0">
            <h2 :id="`${c.id}-name`" class="text-base font-semibold">{{ c.info.name }}</h2>
            <p class="mt-0.5 text-xs text-ink-2">
              {{ BACKEND[c.info.backend] }} · {{ TYPE[c.info.type] }}
              <template v-if="c.info.vendorId !== undefined">
                · <span class="font-mono">{{ hex(c.info.vendorId) }}:{{ hex(c.info.productId) }}</span>
              </template>
              · {{ capabilities(c) }}
            </p>
            <p class="mt-0.5 text-xs text-ink-3">
              {{ c.slot === null ? "No slot: an environment takes four controllers" : `Slot ${c.slot + 1} in an environment` }}
            </p>
          </div>
          <div class="flex shrink-0 flex-wrap gap-2">
            <button
              v-if="c.info.capabilities.rumble"
              type="button"
              class="btn-ghost px-3 py-1.5 text-xs"
              :aria-label="`Test rumble on ${c.info.name}`"
              @click="rumble(c)"
            >
              Test rumble
            </button>
            <button
              v-if="c.info.capabilities.lightbar"
              type="button"
              class="btn-ghost px-3 py-1.5 text-xs"
              :aria-label="`Test lightbar on ${c.info.name}: change its colour`"
              @click="testLight(c)"
            >
              Test lightbar
            </button>
            <button
              v-if="c.info.capabilities.triggers"
              type="button"
              class="btn-ghost px-3 py-1.5 text-xs"
              :aria-label="`Test triggers on ${c.info.name}: they resist for a few seconds`"
              @click="testTriggers(c)"
            >
              Test triggers
            </button>
          </div>
        </div>

        <WarningNote
          v-if="!c.info.mapped"
          class="mt-3"
          message="This controller's buttons aren't in a standard layout, so which is which is a guess. Connecting it with WebHID can fix that."
        />

        <div class="mt-4 flex flex-wrap items-start gap-x-8 gap-y-4">
          <div class="min-w-60 flex-1">
            <ControllerDiagram v-if="live[c.id]" :state="live[c.id]!.state" :type="c.info.type" />
            <p class="mt-2 text-xs text-ink-2">Held: {{ pressed(live[c.id]?.state, c.info.type) }}</p>
          </div>

          <div v-if="live[c.id]" class="min-w-52 space-y-3 text-xs text-ink-2">
            <dl class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 tabular-nums">
              <template v-for="(name, i) in AXES" :key="name">
                <dt>{{ name }}</dt>
                <dd class="font-mono text-ink">{{ num(live[c.id]!.state.axes[i]) }}</dd>
              </template>
              <dt>Left trigger</dt>
              <dd class="font-mono text-ink">{{ pct(live[c.id]!.state.buttons[6]) }}</dd>
              <dt>Right trigger</dt>
              <dd class="font-mono text-ink">{{ pct(live[c.id]!.state.buttons[7]) }}</dd>
              <template v-if="live[c.id]!.state.battery !== undefined">
                <dt>Battery</dt>
                <dd class="font-mono text-ink">{{ pct(live[c.id]!.state.battery) }}</dd>
              </template>
              <template v-if="live[c.id]!.state.gyro">
                <dt>Gyro (rad/s)</dt>
                <dd class="font-mono text-ink">
                  {{ num(live[c.id]!.state.gyro![0]) }} {{ num(live[c.id]!.state.gyro![1]) }}
                  {{ num(live[c.id]!.state.gyro![2]) }}
                </dd>
              </template>
              <template v-if="live[c.id]!.state.accel">
                <dt>Accel (m/s²)</dt>
                <dd class="font-mono text-ink">
                  {{ num(live[c.id]!.state.accel![0], 1) }} {{ num(live[c.id]!.state.accel![1], 1) }}
                  {{ num(live[c.id]!.state.accel![2], 1) }}
                </dd>
              </template>
            </dl>

            <template v-if="c.info.capabilities.touchpad && live[c.id]!.state.touch">
              <div class="flex flex-wrap items-end gap-3" aria-hidden="true">
                <figure v-for="a in areas(c, live[c.id]!.state)" :key="a.label" class="space-y-1">
                  <svg
                    :viewBox="a.wide ? '0 0 100 56' : '0 0 100 100'"
                    :class="[a.wide ? 'w-36' : 'size-20', a.clicked ? 'border-accent' : 'border-line']"
                    class="rounded-lg border bg-canvas"
                  >
                    <circle v-for="(d, i) in a.dots" :key="i" :cx="d.x * 100" :cy="d.y * (a.wide ? 56 : 100)" r="5" class="fill-accent" />
                  </svg>
                  <figcaption class="text-ink-3">{{ a.label }}</figcaption>
                </figure>
              </div>
              <p class="text-ink-3">Touching: {{ live[c.id]!.state.touch!.length || "nothing" }}</p>
            </template>
          </div>
        </div>

        <div v-if="showRaw" class="mt-4 border-t border-line pt-3 text-xs">
          <template v-if="rawGamepad(c.id)">
            <p class="text-ink-2">Gamepad API: mapping "{{ rawGamepad(c.id)!.mapping }}"</p>
            <p class="mt-2 font-medium text-ink-2">Buttons</p>
            <p class="mt-1 font-mono break-words text-ink">
              <span v-for="(v, i) in rawGamepad(c.id)!.buttons" :key="i" class="mr-3 inline-block whitespace-nowrap">
                <span class="text-ink-3">{{ i }}</span> {{ v.toFixed(2) }}
              </span>
            </p>
            <p class="mt-2 font-medium text-ink-2">Axes</p>
            <p class="mt-1 font-mono break-words text-ink">
              <span v-for="(v, i) in rawGamepad(c.id)!.axes" :key="i" class="mr-3 inline-block whitespace-nowrap">
                <span class="text-ink-3">{{ i }}</span> {{ v.toFixed(3) }}
              </span>
            </p>
          </template>
          <template v-else-if="rawHid(c.id)">
            <p class="text-ink-2">
              Last input report: id {{ rawHid(c.id)!.reportId }}, {{ rawHid(c.id)!.bytes.length }} bytes
            </p>
            <pre class="mt-2 font-mono break-words whitespace-pre-wrap text-ink">{{ hexBytes(rawHid(c.id)!.bytes) }}</pre>
          </template>
          <p v-else class="text-ink-3">Nothing received yet: press a button.</p>
        </div>
      </li>
    </ul>

    <AppControllerKinds />
  </div>
</template>
