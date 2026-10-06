<script setup lang="ts">
import type { HealthAssessment, HealthIssue, ProbeResult, StatsSnapshot } from "@cha/player";
import { computed, onBeforeUnmount, ref, useId, useTemplateRef, watch } from "vue";

import { clampPos, codecTag, cornerByArrow, nearestCorner, num, OPACITY_MIN, type Corner, type OverlayPrefs, type SectionId } from "../statsOverlay";

// The session's stats panel: a header (grade, move, fold, close) over either one compact line or
// the full sections. The prefs (open, compact, collapsed, corner) belong to the parent, which
// saves them and lets the toolbar's Stats button bring a closed panel back.
const props = defineProps<{
  stats: StatsSnapshot | null;
  health: HealthAssessment;
  /** The codec chosen, until the stream says what it decodes. */
  codec: string;
  transport: "webtransport" | "webrtc" | null;
  connected: boolean;
  recordingLeft: number | null;
  probe: ProbeResult | null;
  /** The toolbar is showing: the top corners sit below it; folded, they take its space. */
  toolbarOpen?: boolean;
}>();
const prefs = defineModel<OverlayPrefs>({ required: true });
const emit = defineEmits<{ record: [] }>();

const isFolded = (id: SectionId) => prefs.value.folded.includes(id);
const toggleSection = (id: SectionId) =>
  patch({ folded: isFolded(id) ? prefs.value.folded.filter((x) => x !== id) : [...prefs.value.folded, id] });
const patch = (p: Partial<OverlayPrefs>) => (prefs.value = { ...prefs.value, ...p });

const GRADE_TEXT: Record<string, string> = { A: "text-ok", B: "text-ok", C: "text-warn", D: "text-warn", F: "text-danger" };
const gradeClass = computed(() => (props.health.grade ? GRADE_TEXT[props.health.grade] : "text-ink-2"));
const SEVERITY_TEXT = { minor: "text-ink-2", major: "text-warn", critical: "text-danger" };

/** A value is coloured only when health.ts found its signal bad, in that issue's severity. */
function bad(...ids: string[]): string {
  const issue = props.health.issues.find((i) => ids.includes(i.id));
  return issue ? (issue.severity === "minor" ? "text-warn" : SEVERITY_TEXT[issue.severity]) : "";
}
const gb = (bytes: number) => (bytes / 1024 ** 3).toFixed(1);
const pct = (used: number, total: number) => (total > 0 ? (used / total) * 100 : 0);
/** Amber for a node value at or over `limit` percent (°C for the temperature); health.ts only judges the worst of them together. */
const hot = (v: number, limit = 90) => (v >= limit ? "text-warn" : "");

const word = computed(() => props.health.summary);
const codecText = computed(() => codecTag(props.stats?.codec ?? props.codec, props.transport));
const compactLine = computed(() => {
  const s = props.stats;
  return [`${num(s?.fps ?? null, 0)} fps`, `${num(s?.latencyMs ?? null)} ms`, `${num(s?.mbps ?? null)} Mbit/s`, codecText.value].join(" · ");
});
/** What a folded section still shows on its heading: its headline numbers, amber when health.ts found them bad. */
const summary = computed<Record<SectionId, { text: string; cls: string }>>(() => {
  const s = props.stats;
  const n = node.value;
  return {
    stream: { text: [codecText.value, `${num(s?.fps ?? null, 0)} fps`, `${num(s?.mbps ?? null)} Mbit/s`].filter(Boolean).join(" · "), cls: bad("stutter", "freeze") },
    latency: { text: `${num(s?.latencyMs ?? null)} ms · decode ${num(s?.decodeMs ?? null)} ms`, cls: bad("latency", "decode") },
    network: { text: `${num(s?.rttMs ?? null)} ms · ${s?.packetsLost ?? 0} lost · ${s?.framesDropped ?? 0} dropped`, cls: bad("rtt", "loss", "recovered", "dropped") },
    node: {
      text: n ? [`CPU ${num(n.cpu, 0)}%`, n.gpu !== undefined ? `GPU ${num(n.gpu, 0)}%` : "", n.temp !== undefined ? `${n.temp} °C` : ""].filter(Boolean).join(" · ") : "",
      cls: n && (hot(n.cpu) || (n.gpu !== undefined && hot(n.gpu)) || (n.temp !== undefined && hot(n.temp, 85))) ? "text-warn" : "",
    },
  };
});
// ---- Copying a warning with the numbers around it, for a bug report or a chat ----

const copiedId = ref<string | null>(null);
let copiedTimer: ReturnType<typeof setTimeout> | undefined;
function issueReport(issue: HealthIssue): string {
  const s = props.stats;
  const n = node.value;
  const ms = (v: number | null | undefined, d = 1) => `${num(v, d)} ms`;
  const lines = [
    `${issue.title}: ${issue.detail}`,
    issue.hint,
    "",
    `Health: ${props.health.grade ?? "not measured"}${props.health.score !== null ? ` (${props.health.score}/100)` : ""}, ${props.health.summary}`,
    `Stream: ${codecText.value || "–"}, ${s?.width ?? "–"}×${s?.height ?? "–"}, ${num(s?.fps ?? null, 0)}${s?.targetFps ? ` of ${s.targetFps}` : ""} fps, ${num(s?.mbps ?? null)} Mbit/s`,
    `Latency: send → shown ${ms(s?.latencyMs)}, decode ${ms(s?.decodeMs, 2)}, jitter buffer ${ms(s?.jitterMs, 2)}, audio buffer ${ms(s?.audioJitterMs, 0)}`,
    `Network: round trip ${ms(s?.rttMs)}, ${s?.packetsLost ?? 0} lost, ${s?.framesDropped ?? 0} dropped`,
  ];
  if (n) {
    const parts = [`CPU ${num(n.cpu, 0)}%`, `RAM ${gb(n.memUsed)}/${gb(n.memTotal)} GB`];
    if (n.gpu !== undefined) parts.push(`GPU ${num(n.gpu, 0)}%`);
    if (n.vramUsed !== undefined && n.vramTotal !== undefined) parts.push(`VRAM ${gb(n.vramUsed)}/${gb(n.vramTotal)} GB`);
    if (n.temp !== undefined) parts.push(`${n.temp} °C`);
    if (n.power !== undefined) parts.push(`${num(n.power, 0)}${n.powerLimit ? `/${num(n.powerLimit, 0)}` : ""} W`);
    lines.push(`Node: ${parts.join(", ")}`);
  }
  lines.push(`Browser: ${navigator.userAgent}`);
  return lines.join("\n");
}
async function copyIssue(issue: HealthIssue) {
  try {
    await navigator.clipboard.writeText(issueReport(issue));
    copiedId.value = issue.id;
  } catch {
    copiedId.value = null; // clipboard blocked: nothing to confirm
    return;
  }
  clearTimeout(copiedTimer);
  copiedTimer = setTimeout(() => (copiedId.value = null), 1500);
}
onBeforeUnmount(() => clearTimeout(copiedTimer));

const topIssue = computed(() => props.health.issues[0] ?? null);
const node = computed(() => props.stats?.node ?? null);

// ---- Moving: drag the header, or use the arrow keys on it; it snaps to the nearest corner ----

// The top corners sit below the toolbar (top-3, 42 px tall), which is wider than the gap beside it
// on most laptop screens.
const CORNER_CLASS = computed<Record<Corner, string>>(() => {
  const top = props.toolbarOpen ? "top-16" : "top-3";
  return {
    "top-left": `${top} left-3`,
    "top-right": `${top} right-3`,
    "bottom-left": "bottom-3 left-3",
    "bottom-right": "bottom-3 right-3",
  };
});
const panel = useTemplateRef<HTMLElement>("panel");
/** Where the panel is while it is being dragged, in its container's pixels. */
const dragAt = ref<{ left: number; top: number } | null>(null);
let grab: { dx: number; dy: number; id: number } | null = null;

function container() {
  return panel.value?.offsetParent as HTMLElement | null;
}
function onDown(e: PointerEvent) {
  const el = panel.value;
  const parent = container();
  if (!el || !parent || (e.pointerType === "mouse" && e.button !== 0)) return;
  const r = el.getBoundingClientRect();
  const p = parent.getBoundingClientRect();
  grab = { dx: e.clientX - r.left, dy: e.clientY - r.top, id: e.pointerId };
  (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  dragAt.value = { left: r.left - p.left, top: r.top - p.top };
}
function onMove(e: PointerEvent) {
  const parent = container();
  const el = panel.value;
  if (!grab || grab.id !== e.pointerId || !parent || !el) return;
  const p = parent.getBoundingClientRect();
  dragAt.value = {
    left: Math.min(Math.max(0, e.clientX - p.left - grab.dx), Math.max(0, p.width - el.offsetWidth)),
    top: Math.min(Math.max(0, e.clientY - p.top - grab.dy), Math.max(0, p.height - el.offsetHeight)),
  };
}
function onUp(e: PointerEvent) {
  const parent = container();
  const el = panel.value;
  if (!grab || grab.id !== e.pointerId) return;
  grab = null;
  if (parent && el && dragAt.value) {
    if (prefs.value.snap) {
      const corner = nearestCorner(dragAt.value.left + el.offsetWidth / 2, dragAt.value.top + el.offsetHeight / 2, parent.clientWidth, parent.clientHeight);
      patch({ corner });
    } else {
      patch({ pos: { left: Math.round(dragAt.value.left), top: Math.round(dragAt.value.top) } });
    }
  }
  dragAt.value = null;
}
function onKey(e: KeyboardEvent) {
  if (!e.key.startsWith("Arrow")) return;
  e.preventDefault();
  if (prefs.value.snap) {
    patch({ corner: cornerByArrow(prefs.value.corner, e.key) });
    return;
  }
  // Free placement: nudge 16 px (64 with Shift), from where it is now.
  const el = panel.value;
  const parent = container();
  if (!el || !parent) return;
  const step = e.shiftKey ? 64 : 16;
  const from = freePos.value ?? { left: el.offsetLeft, top: el.offsetTop };
  const dx = e.key === "ArrowLeft" ? -step : e.key === "ArrowRight" ? step : 0;
  const dy = e.key === "ArrowUp" ? -step : e.key === "ArrowDown" ? step : 0;
  patch({ pos: clampPos({ left: from.left + dx, top: from.top + dy }, el.offsetWidth, el.offsetHeight, parent.clientWidth, parent.clientHeight) });
}

// Free placement is clamped when it is drawn, not only when it is dropped, so a saved spot that is
// off screen (a smaller window, a reload, the compact view growing) is pulled back inside. The
// saved value is left alone, so it returns if the window grows again.
const area = ref({ w: 0, h: 0 });
const size = ref({ w: 0, h: 0 });
const freePos = computed(() => {
  const pos = prefs.value.pos;
  if (prefs.value.snap || !pos || !area.value.w) return null;
  return clampPos(pos, size.value.w, size.value.h, area.value.w, area.value.h);
});
let watching: ResizeObserver | null = null;
function measure() {
  const el = panel.value;
  const parent = container();
  if (el) size.value = { w: el.offsetWidth, h: el.offsetHeight };
  if (parent) area.value = { w: parent.clientWidth, h: parent.clientHeight };
}
watch(
  panel,
  (el) => {
    watching?.disconnect();
    watching = null;
    if (!el) return;
    measure();
    watching = new ResizeObserver(measure);
    watching.observe(el);
    if (el.offsetParent) watching.observe(el.offsetParent);
  },
  { flush: "post" },
);
onBeforeUnmount(() => watching?.disconnect());

const menuOpen = ref(false);
// The settings strip closes itself: when the panel is folded or hidden, and after a minute with
// no touch on it (any use of the strip starts the minute again).
const MENU_IDLE_MS = 60_000;
let menuTimer: ReturnType<typeof setTimeout> | undefined;
function keepMenu() {
  clearTimeout(menuTimer);
  if (menuOpen.value) menuTimer = setTimeout(() => (menuOpen.value = false), MENU_IDLE_MS);
}
watch(menuOpen, keepMenu);
watch(
  () => prefs.value.collapsed || !prefs.value.open,
  (gone) => {
    if (gone) menuOpen.value = false;
  },
);
onBeforeUnmount(() => clearTimeout(menuTimer));
// The more see-through the background, the more the video shows behind the text. So as opacity
// drops, the dimmer text moves toward full ink and gets a dark halo, keeping it legible.
const panelStyle = computed(() => {
  const o = prefs.value.opacity;
  const t = Math.min(1, Math.max(0, (100 - o) / (100 - OPACITY_MIN)));
  const style: Record<string, string> = {
    backgroundColor: `color-mix(in oklab, var(--cha-panel) ${o}%, transparent)`,
    "--cha-ink-2": `color-mix(in oklab, var(--cha-ink-2-theme), var(--cha-ink) ${Math.round(t * 100)}%)`,
    textShadow: `0 0 ${(2 + 4 * t).toFixed(1)}px rgb(0 0 0 / ${(0.35 + 0.55 * t).toFixed(2)}), 0 1px 1px rgb(0 0 0 / ${(0.5 * t).toFixed(2)})`,
  };
  const at = dragAt.value ?? freePos.value;
  if (at) {
    style.left = `${at.left}px`;
    style.top = `${at.top}px`;
  }
  return style;
});
const placedFree = computed(() => !!dragAt.value || !!freePos.value);

const bodyId = useId();
const menuId = useId();
const BTN =
  "grid size-5 place-items-center rounded text-ink-2 hover:bg-line/60 hover:text-ink focus-visible:outline-2 focus-visible:outline-focus";
</script>

<template>
  <!-- Hidden: a faint grade chip in the same corner, one click from the panel (no toolbar needed) -->
  <button
    v-if="!prefs.open"
    type="button"
    class="absolute z-10 rounded-md border border-line bg-panel/60 px-1.5 font-mono text-2xs leading-5 opacity-30 backdrop-blur-md transparency-reduced:bg-panel transparency-reduced:backdrop-blur-none transition-opacity hover:opacity-100 focus-visible:opacity-100 focus-visible:outline-2 focus-visible:outline-focus"
    :class="CORNER_CLASS[prefs.corner]"
    :aria-label="`Show the stats panel, stream health ${health.grade ?? 'not measured'}`"
    title="Show stats"
    @click="patch({ open: true })"
  >
    <span class="font-semibold" :class="gradeClass">{{ health.grade ?? "–" }}</span>
  </button>
  <section
    v-else
    ref="panel"
    aria-label="Stream statistics"
    class="absolute z-10 max-h-[calc(100%-5rem)] max-w-[calc(100%-1.5rem)] flex flex-col overflow-hidden rounded-lg border border-line font-mono text-2xs leading-5 text-ink shadow-lg backdrop-blur-md transparency-reduced:backdrop-blur-none [font-variant-numeric:tabular-nums]"
    :class="[placedFree ? '' : CORNER_CLASS[prefs.corner], prefs.compact || prefs.collapsed ? 'w-max' : 'w-64']"
    :style="panelStyle"
    @keydown.esc="menuOpen = false"
  >
    <!-- Header -->
    <div class="flex h-6 shrink-0 items-center gap-1 pr-1">
      <button
        type="button"
        class="flex h-6 min-w-0 flex-1 cursor-grab touch-none items-center gap-1.5 rounded-tl-lg px-2 text-left select-none focus-visible:outline-2 focus-visible:outline-focus active:cursor-grabbing"
        :aria-label="`Stats panel, stream health ${health.grade ?? 'not measured'}, ${word}. Drag or use the arrow keys to move it`"
        title="Drag to move; arrow keys on this handle move it too"
        @pointerdown="onDown"
        @pointermove="onMove"
        @pointerup="onUp"
        @pointercancel="onUp"
        @keydown="onKey"
      >
        <span class="font-semibold" :class="gradeClass">{{ health.grade ?? "–" }}</span>
        <span v-if="prefs.collapsed" class="whitespace-nowrap text-ink" :title="word">{{ compactLine }}</span>
        <span v-else class="truncate text-ink-2" :title="word">{{ word }}</span>
      </button>
      <button
        type="button"
        :class="BTN"
        :aria-label="prefs.compact ? 'Show full stats' : 'Show compact stats'"
        :title="prefs.compact ? 'Full view' : 'Compact view'"
        @click="patch({ compact: !prefs.compact })"
      >
        <svg viewBox="0 0 16 16" class="size-3" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path v-if="prefs.compact" d="M3 6V3h3M13 6V3h-3M3 10v3h3M13 10v3h-3" />
          <path v-else d="M6 3v3H3M10 3v3h3M6 13v-3H3M10 13v-3h3" />
        </svg>
      </button>
      <button
        type="button"
        :class="BTN"
        :aria-label="prefs.collapsed ? 'Expand the stats panel' : 'Collapse the stats panel'"
        :aria-expanded="!prefs.collapsed"
        :aria-controls="bodyId"
        :title="prefs.collapsed ? 'Expand' : 'Collapse to the header'"
        @click="patch({ collapsed: !prefs.collapsed })"
      >
        <svg viewBox="0 0 16 16" class="size-3" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path :d="prefs.collapsed ? 'M4 6l4 4 4-4' : 'M4 10l4-4 4 4'" />
        </svg>
      </button>
      <button
        type="button"
        :class="[BTN, menuOpen && 'bg-line/60 text-ink']"
        aria-label="Panel settings"
        aria-haspopup="true"
        :aria-expanded="menuOpen"
        :aria-controls="menuId"
        title="Panel settings"
        @click="menuOpen = !menuOpen"
      >
        <svg viewBox="0 0 16 16" class="size-3" fill="currentColor" aria-hidden="true">
          <circle cx="8" cy="3" r="1.3" /><circle cx="8" cy="8" r="1.3" /><circle cx="8" cy="13" r="1.3" />
        </svg>
      </button>
      <button type="button" :class="BTN" aria-label="Hide the stats panel" title="Hide (click the grade chip or the toolbar's Stats button to bring it back)" @click="patch({ open: false })">
        <svg viewBox="0 0 16 16" class="size-3" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" aria-hidden="true">
          <path d="M2 8s2.2-4 6-4 6 4 6 4-2.2 4-6 4-6-4-6-4z" />
          <circle cx="8" cy="8" r="1.6" />
          <path d="M3 13L13 3" />
        </svg>
      </button>
    </div>

    <div v-if="menuOpen" :id="menuId" @pointermove="keepMenu" @input="keepMenu" @keydown="keepMenu" class="shrink-0 space-y-1.5 border-t border-line px-2 py-1.5">
      <label class="flex items-center gap-2" title="How see-through the panel's background is. The text stays solid.">
        <span class="text-ink-2">Opacity</span>
        <input
          type="range"
          :min="OPACITY_MIN"
          max="100"
          step="5"
          class="min-w-0 flex-1 accent-accent"
          :value="prefs.opacity"
          :aria-valuetext="`${prefs.opacity}%`"
          @input="patch({ opacity: Number(($event.target as HTMLInputElement).value) })"
        />
        <span class="w-8 text-right">{{ prefs.opacity }}%</span>
      </label>
      <label class="flex items-center gap-2" title="On: dropping the panel snaps it to the nearest corner. Off: it stays exactly where you drop it.">
        <input type="checkbox" class="accent-accent" :checked="prefs.snap" @change="patch({ snap: ($event.target as HTMLInputElement).checked })" />
        <span>Snap to corners</span>
      </label>
    </div>

    <div v-show="!prefs.collapsed" :id="bodyId" class="min-h-0 overflow-x-hidden overflow-y-auto border-t border-line px-2 pt-1 pb-1.5">
      <!-- Compact: one line, and the top reason when the grade isn't an A -->
      <template v-if="prefs.compact">
        <div class="whitespace-nowrap text-ink">
          <span class="font-semibold" :class="gradeClass">{{ health.grade ?? "–" }}</span> · {{ compactLine }}
        </div>
        <div v-if="topIssue" class="flex max-w-72 items-center">
          <span class="truncate text-ink-2" :title="`${topIssue.title}: ${topIssue.detail}`">{{ topIssue.title }} · {{ topIssue.detail }}</span>
          <button
            type="button"
            class="ml-1 grid size-4 shrink-0 place-items-center rounded align-middle text-ink-2 hover:bg-line/60 hover:text-ink focus-visible:outline-2 focus-visible:outline-focus"
            :aria-label="copiedId === topIssue.id ? 'Copied' : 'Copy this warning with the stream numbers'"
            :title="copiedId === topIssue.id ? 'Copied' : 'Copy this warning and the stream numbers'"
            @click="copyIssue(topIssue)"
          >
            <svg viewBox="0 0 16 16" class="size-3" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path v-if="copiedId === topIssue.id" d="M3 8.5l3.2 3L13 4.5" />
              <template v-else><rect x="5.5" y="5.5" width="7" height="8" rx="1.2" /><path d="M10.5 3.5v-.3a.7.7 0 0 0-.7-.7H4.2a.7.7 0 0 0-.7.7v7.1c0 .4.3.7.7.7h.3" /></template>
            </svg>
          </button>
        </div>
      </template>

      <!-- Full -->
      <template v-else>
        <div v-if="health.issues.length" class="mb-1 space-y-1">
          <div v-for="issue in health.issues" :key="issue.id">
            <div><span :class="SEVERITY_TEXT[issue.severity]">{{ issue.title }}</span> <span class="text-ink-2">· {{ issue.detail }}</span><button
            type="button"
            class="ml-1 inline-grid size-4 shrink-0 place-items-center rounded align-middle text-ink-2 hover:bg-line/60 hover:text-ink focus-visible:outline-2 focus-visible:outline-focus"
            :aria-label="copiedId === issue.id ? 'Copied' : 'Copy this warning with the stream numbers'"
            :title="copiedId === issue.id ? 'Copied' : 'Copy this warning and the stream numbers'"
            @click="copyIssue(issue)"
          >
            <svg viewBox="0 0 16 16" class="size-3" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path v-if="copiedId === issue.id" d="M3 8.5l3.2 3L13 4.5" />
              <template v-else><rect x="5.5" y="5.5" width="7" height="8" rx="1.2" /><path d="M10.5 3.5v-.3a.7.7 0 0 0-.7-.7H4.2a.7.7 0 0 0-.7.7v7.1c0 .4.3.7.7.7h.3" /></template>
            </svg>
          </button></div>
            <div class="text-ink-2">{{ issue.hint }}</div>
          </div>
        </div>
        <div v-if="health.score !== null" class="mb-1 text-ink-2">Health {{ health.score }}/100</div>

        <h3 class="mt-1.5 text-2xs font-semibold tracking-wider text-accent uppercase">
          <button type="button" class="flex w-full items-center gap-1 uppercase hover:brightness-125 focus-visible:outline-2 focus-visible:outline-focus" :aria-expanded="!isFolded('stream')" @click="toggleSection('stream')">
            <svg viewBox="0 0 16 16" class="size-2.5 transition-transform" :class="isFolded('stream') && '-rotate-90'" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 6l4 4 4-4" /></svg>
            Stream
            <span v-if="isFolded('stream')" class="ml-auto min-w-0 truncate text-right font-normal tracking-normal normal-case" :class="summary.stream.cls || 'text-ink'">{{ summary.stream.text }}</span>
          </button>
        </h3>
        <dl v-show="!isFolded('stream')" class="grid grid-cols-[1fr_auto] [&>dt]:flex [&>dt]:items-center [&>dt]:text-ink-2 [&>dt]:whitespace-nowrap [&>dt]:after:ml-1.5 [&>dt]:after:h-px [&>dt]:after:flex-1 [&>dt]:after:bg-ink-2/50 [&>dd]:flex [&>dd]:items-center [&>dd]:justify-end [&>dd]:whitespace-pre [&>dd]:text-ink [&>dd]:before:mr-1.5 [&>dd]:before:h-px [&>dd]:before:flex-1 [&>dd]:before:bg-ink-2/50">
          <dt title="How the video is compressed on the node. The suffix is how it travels: WT is WebTransport, RTC is WebRTC.">Codec</dt><dd>{{ codecText || "–" }}</dd>
          <dt title="The resolution of the picture the browser is decoding, in pixels.">Size</dt><dd>{{ stats?.width ?? "–" }}×{{ stats?.height ?? "–" }}</dd>
          <dt title="Frames per second arriving here, against the rate the node encodes at. A still screen sends few frames, so a low number on an idle desktop is normal.">Frame rate</dt><dd :class="bad('stutter', 'freeze')">{{ num(stats?.fps ?? null, 0) }}<template v-if="stats?.targetFps"> of {{ stats.targetFps }}</template> fps</dd>
          <dt title="How much video data is arriving each second, in megabits. It rises with motion and falls on a still screen.">Bitrate</dt><dd>{{ num(stats?.mbps ?? null) }} Mbit/s</dd>
        </dl>

        <h3 class="mt-1.5 text-2xs font-semibold tracking-wider text-chart-1 uppercase">
          <button type="button" class="flex w-full items-center gap-1 uppercase hover:brightness-125 focus-visible:outline-2 focus-visible:outline-focus" :aria-expanded="!isFolded('latency')" @click="toggleSection('latency')">
            <svg viewBox="0 0 16 16" class="size-2.5 transition-transform" :class="isFolded('latency') && '-rotate-90'" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 6l4 4 4-4" /></svg>
            Latency
            <span v-if="isFolded('latency')" class="ml-auto min-w-0 truncate text-right font-normal tracking-normal normal-case" :class="summary.latency.cls || 'text-ink'">{{ summary.latency.text }}</span>
          </button>
        </h3>
        <dl v-show="!isFolded('latency')" class="grid grid-cols-[1fr_auto] [&>dt]:flex [&>dt]:items-center [&>dt]:text-ink-2 [&>dt]:whitespace-nowrap [&>dt]:after:ml-1.5 [&>dt]:after:h-px [&>dt]:after:flex-1 [&>dt]:after:bg-ink-2/50 [&>dd]:flex [&>dd]:items-center [&>dd]:justify-end [&>dd]:whitespace-pre [&>dd]:text-ink [&>dd]:before:mr-1.5 [&>dd]:before:h-px [&>dd]:before:flex-1 [&>dd]:before:bg-ink-2/50">
          <dt title="Time from the node sending a frame to it appearing on your screen (the median over the last second). It adds network, buffering and decoding, but not the time the app took to react to your input.">Send → shown</dt><dd :class="bad('latency')">{{ num(stats?.latencyMs ?? null) }} ms</dd>
          <dt title="Average time your browser takes to decode one frame. If this approaches the gap between frames, the picture will stutter.">Decode</dt><dd :class="bad('decode')">{{ num(stats?.decodeMs ?? null, 2) }} ms</dd>
          <dt title="Average time each frame waits in the browser to even out uneven network arrival. More wait is smoother video but adds delay.">Jitter buffer</dt><dd :class="bad('jitter')">{{ num(stats?.jitterMs ?? null, 2) }} ms</dd>
          <dt title="Average time sound waits in the browser's audio buffer to avoid crackles. More wait is steadier sound but later sound.">Audio buffer</dt><dd :class="bad('audio')">{{ num(stats?.audioJitterMs ?? null, 0) }} ms</dd>
        </dl>

        <h3 class="mt-1.5 text-2xs font-semibold tracking-wider text-chart-2 uppercase">
          <button type="button" class="flex w-full items-center gap-1 uppercase hover:brightness-125 focus-visible:outline-2 focus-visible:outline-focus" :aria-expanded="!isFolded('network')" @click="toggleSection('network')">
            <svg viewBox="0 0 16 16" class="size-2.5 transition-transform" :class="isFolded('network') && '-rotate-90'" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 6l4 4 4-4" /></svg>
            Network
            <span v-if="isFolded('network')" class="ml-auto min-w-0 truncate text-right font-normal tracking-normal normal-case" :class="summary.network.cls || 'text-ink'">{{ summary.network.text }}</span>
          </button>
        </h3>
        <dl v-show="!isFolded('network')" class="grid grid-cols-[1fr_auto] [&>dt]:flex [&>dt]:items-center [&>dt]:text-ink-2 [&>dt]:whitespace-nowrap [&>dt]:after:ml-1.5 [&>dt]:after:h-px [&>dt]:after:flex-1 [&>dt]:after:bg-ink-2/50 [&>dd]:flex [&>dd]:items-center [&>dd]:justify-end [&>dd]:whitespace-pre [&>dd]:text-ink [&>dd]:before:mr-1.5 [&>dd]:before:h-px [&>dd]:before:flex-1 [&>dd]:before:bg-ink-2/50">
          <dt title="How long a message takes to reach the node and come back. It is the network's base delay, with no processing included.">Round trip</dt><dd :class="bad('rtt')">{{ num(stats?.rttMs ?? null) }} ms</dd>
          <dt title="Network packets that never arrived. A few are harmless: lost video data is rebuilt from spare data or skipped.">Lost</dt><dd :class="bad('loss', 'recovered')">{{ stats?.packetsLost ?? 0 }}</dd>
          <dt title="Frames that arrived but were thrown away instead of shown, usually because they came too late or the browser fell behind.">Dropped</dt><dd :class="bad('dropped')">{{ stats?.framesDropped ?? 0 }}</dd>
        </dl>

        <template v-if="node">
          <h3 class="mt-1.5 text-2xs font-semibold tracking-wider text-chart-3 uppercase">
          <button type="button" class="flex w-full items-center gap-1 uppercase hover:brightness-125 focus-visible:outline-2 focus-visible:outline-focus" :aria-expanded="!isFolded('node')" @click="toggleSection('node')">
            <svg viewBox="0 0 16 16" class="size-2.5 transition-transform" :class="isFolded('node') && '-rotate-90'" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 6l4 4 4-4" /></svg>
            Node
            <span v-if="isFolded('node')" class="ml-auto min-w-0 truncate text-right font-normal tracking-normal normal-case" :class="summary.node.cls || 'text-ink'">{{ summary.node.text }}</span>
          </button>
        </h3>
          <dl v-show="!isFolded('node')" class="grid grid-cols-[1fr_auto] [&>dt]:flex [&>dt]:items-center [&>dt]:text-ink-2 [&>dt]:whitespace-nowrap [&>dt]:after:ml-1.5 [&>dt]:after:h-px [&>dt]:after:flex-1 [&>dt]:after:bg-ink-2/50 [&>dd]:flex [&>dd]:items-center [&>dd]:justify-end [&>dd]:whitespace-pre [&>dd]:text-ink [&>dd]:before:mr-1.5 [&>dd]:before:h-px [&>dd]:before:flex-1 [&>dd]:before:bg-ink-2/50">
            <dt title="How busy the node's processor is overall, across all cores.">CPU</dt><dd :class="hot(node.cpu)">{{ num(node.cpu, 0) }}%</dd>
            <template v-if="node.cores"><dt title="The node's average number of busy processes over the last minute, next to its core count. Steadily above the core count means it is overloaded.">Load</dt><dd>{{ num(node.load1) }} <span class="text-ink-2">on {{ node.cores }} cores</span></dd></template>
            <dt title="Memory in use on the node out of its total.">RAM</dt>
            <dd><span :class="hot(pct(node.memUsed, node.memTotal))">{{ gb(node.memUsed) }}</span> / {{ gb(node.memTotal) }} GB</dd>
            <template v-if="node.gpu !== undefined"><dt title="How busy the node's graphics card is. Games and the desktop's rendering use this.">GPU</dt><dd :class="hot(node.gpu)">{{ num(node.gpu, 0) }}%</dd></template>
            <template v-if="node.vramUsed !== undefined && node.vramTotal !== undefined">
              <dt title="Graphics card memory in use out of its total.">VRAM</dt>
              <dd><span :class="hot(pct(node.vramUsed, node.vramTotal))">{{ gb(node.vramUsed) }}</span> / {{ gb(node.vramTotal) }} GB</dd>
            </template>
            <template v-if="node.temp !== undefined"><dt title="The graphics card's temperature. Much above 85 °C it may slow itself down.">Temp</dt><dd :class="hot(node.temp, 85)">{{ node.temp }} °C</dd></template>
            <template v-if="node.power !== undefined">
              <dt title="What the graphics card is drawing now, out of the limit it is allowed.">Power</dt>
              <dd><span :class="node.powerLimit ? hot(pct(node.power, node.powerLimit)) : ''">{{ num(node.power, 0) }}</span><template v-if="node.powerLimit"> / {{ num(node.powerLimit, 0) }}</template> W</dd>
            </template>
            <template v-if="node.clock !== undefined"><dt title="The graphics card's current core speed. It drops when the card is idle, hot or at its power limit.">Clock</dt><dd>{{ node.clock }} MHz</dd></template>
            <template v-if="node.enc !== undefined"><dt title="How busy the card's video encoder is. This is the part that compresses the stream.">NVENC</dt><dd :class="hot(node.enc)">{{ num(node.enc, 0) }}%</dd></template>
            <template v-if="node.dec !== undefined"><dt title="How busy the card's video decoder is. It is used when the environment itself plays video.">NVDEC</dt><dd :class="hot(node.dec)">{{ num(node.dec, 0) }}%</dd></template>
            <dt title="Processor used by the streaming program alone, in percent of one core, so it can pass 100.">Streamer CPU</dt><dd>{{ num(node.streamerCpu, 0) }}%</dd>
          </dl>
        </template>

        <div v-if="probe" class="mt-1.5 border-t border-line pt-1 text-accent">
          click → shown {{ num(probe.clickToPresentedMs.p50) }} / {{ num(probe.clickToPresentedMs.p95) }} ms ({{ probe.samples }}, {{ probe.missed }} missed)
          <template v-if="probe.audioSamples">
            <br />click → sound {{ num(probe.clickToAudioMs.p50) }} / {{ num(probe.clickToAudioMs.p95) }} ms · A/V {{ num(probe.avOffsetMs.p50) }} ms
          </template>
        </div>

        <div class="mt-1.5 border-t border-line pt-1">
          <button
            type="button"
            class="rounded px-1 text-ink-2 hover:text-ink focus-visible:outline-2 focus-visible:outline-focus disabled:cursor-not-allowed disabled:opacity-50"
            :disabled="!connected || recordingLeft !== null"
            title="Collect 30 s of stats, then copy a summary"
            @click="emit('record')"
          >
            {{ recordingLeft === null ? "Record 30 s" : `Recording… ${recordingLeft} s` }}
          </button>
        </div>
      </template>
    </div>
  </section>
</template>
