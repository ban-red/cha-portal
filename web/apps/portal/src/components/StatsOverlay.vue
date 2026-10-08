<script setup lang="ts">
import type { HealthAssessment, HealthIssue, ProbeResult, StatsSnapshot } from "@cha/player";
import { buildPanel, panelReport, STATS_PANEL, type SectionColor, type Tone } from "@cha/ui-spec";
import { computed, onBeforeUnmount, ref, useId, useTemplateRef, watch } from "vue";

import Icon from "./Icon.vue";
import { clampPos, cornerByArrow, nearestCorner, num, OPACITY_MIN, type Corner, type OverlayPrefs, type SectionId } from "../statsOverlay";
import { statsValues } from "../statsValues";

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
  /** Times the stream came back on its own after dropping, this visit. */
  reconnects?: number;
  probe: ProbeResult | null;
  /** The height of the toolbar while it shows (0 when folded): the top corners sit below it, or take its space. */
  toolbarInset?: number;
}>();
const prefs = defineModel<OverlayPrefs>({ required: true });
const emit = defineEmits<{ record: [] }>();

const isFolded = (id: SectionId) => prefs.value.folded.includes(id);
const toggleSection = (id: SectionId) =>
  patch({ folded: isFolded(id) ? prefs.value.folded.filter((x) => x !== id) : [...prefs.value.folded, id] });
const patch = (p: Partial<OverlayPrefs>) => (prefs.value = { ...prefs.value, ...p });

// What the panel says (rows, labels, tooltips, summaries, the compact line and the copy report) is
// built from @cha/ui-spec's stats-panel.json by `buildPanel`; this file only draws it. The class
// names are written out so Tailwind sees them.
const TONE_TEXT: Record<Tone, string> = { none: "", ok: "text-ok", warn: "text-warn", danger: "text-danger", dim: "text-ink-2" };
const COLOR_TEXT: Record<SectionColor, string> = { accent: "text-accent", "chart-1": "text-chart-1", "chart-2": "text-chart-2", "chart-3": "text-chart-3" };
const SEVERITY_TEXT = { minor: "text-ink-2", major: "text-warn", critical: "text-danger" };

const panelModel = computed(() => buildPanel(STATS_PANEL, statsValues({ stats: props.stats, codec: props.codec, transport: props.transport, reconnects: props.reconnects }), props.health, "web"));
const gradeClass = computed(() => TONE_TEXT[panelModel.value.gradeTone] || "text-ink");
const word = computed(() => props.health.summary);
const compactLine = computed(() => panelModel.value.compact);

// ---- Copying a warning with the numbers around it, for a bug report or a chat ----

const copiedId = ref<string | null>(null);
let copiedTimer: ReturnType<typeof setTimeout> | undefined;
const issueReport = (issues: HealthIssue[]) => panelReport(panelModel.value, issues, navigator.userAgent);
/** Copy one warning, or all of them (`id` "all"), with the numbers around them. */
async function copyIssues(issues: HealthIssue[], id: string) {
  try {
    await navigator.clipboard.writeText(issueReport(issues));
    copiedId.value = id;
  } catch {
    copiedId.value = null; // clipboard blocked: nothing to confirm
    return;
  }
  clearTimeout(copiedTimer);
  copiedTimer = setTimeout(() => (copiedId.value = null), 1500);
}
const copyIssue = (issue: HealthIssue) => copyIssues([issue], issue.id);
onBeforeUnmount(() => clearTimeout(copiedTimer));

const topIssue = computed(() => props.health.issues[0] ?? null);

// ---- Moving: drag the header, or use the arrow keys on it; it snaps to the nearest corner ----

// The top corners sit below the toolbar (top-3, 42 px tall), which is wider than the gap beside it
// on most laptop screens.
const CORNER_CLASS: Record<Corner, string> = {
  "top-left": "left-3",
  "top-right": "right-3",
  "bottom-left": "bottom-3 left-3",
  "bottom-right": "bottom-3 right-3",
};
/** The top corners' offset: 12 px, or below the toolbar (12 px above it, 10 px gap) while it shows. */
const topStyle = computed(() => (prefs.value.corner.startsWith("top") ? { top: `${props.toolbarInset ? props.toolbarInset + 22 : 12}px` } : {}));
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
  if (!at) Object.assign(style, topStyle.value);
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
    :style="topStyle"
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
    class="absolute z-10 transition-[top] duration-200 max-h-[calc(100%-5rem)] max-w-[calc(100%-1.5rem)] flex flex-col overflow-hidden rounded-lg border border-line font-mono text-2xs leading-5 text-ink shadow-lg backdrop-blur-md transparency-reduced:backdrop-blur-none [font-variant-numeric:tabular-nums]"
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
        <Icon :name="prefs.compact ? 'stats-full' : 'stats-compact'" class="size-3" />
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
        <Icon :name="prefs.collapsed ? 'panel-expand' : 'panel-collapse'" class="size-3" />
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
        <Icon name="settings-dots" class="size-3" />
      </button>
      <button type="button" :class="BTN" aria-label="Hide the stats panel" title="Hide (click the grade chip or the toolbar's Stats button to bring it back)" @click="patch({ open: false })">
        <Icon name="eye-off" class="size-3" />
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
            <Icon :name="copiedId === topIssue.id ? 'check' : 'copy'" class="size-3" />
          </button>
        </div>
      </template>

      <!-- Full -->
      <template v-else>
        <div v-if="health.issues.length > 1" class="mb-0.5 flex items-center justify-between text-ink-2">
          <span>{{ health.issues.length }} warnings</span>
          <button
            type="button"
            class="rounded px-1 hover:bg-line/60 hover:text-ink focus-visible:outline-2 focus-visible:outline-focus"
            title="Copy every warning and the stream numbers"
            @click="copyIssues(health.issues, 'all')"
          >
            {{ copiedId === "all" ? "Copied" : "Copy all" }}
          </button>
        </div>
        <div v-if="health.issues.length" class="mb-1 max-h-40 space-y-1 overflow-y-auto overscroll-contain pr-1" tabindex="0" aria-label="Health warnings">
          <div v-for="issue in health.issues" :key="issue.id">
            <div><span :class="SEVERITY_TEXT[issue.severity]">{{ issue.title }}</span> <span class="text-ink-2">· {{ issue.detail }}</span><button
            type="button"
            class="ml-1 inline-grid size-4 shrink-0 place-items-center rounded align-middle text-ink-2 hover:bg-line/60 hover:text-ink focus-visible:outline-2 focus-visible:outline-focus"
            :aria-label="copiedId === issue.id ? 'Copied' : 'Copy this warning with the stream numbers'"
            :title="copiedId === issue.id ? 'Copied' : 'Copy this warning and the stream numbers'"
            @click="copyIssue(issue)"
          >
            <Icon :name="copiedId === issue.id ? 'check' : 'copy'" class="size-3" />
          </button></div>
            <div class="text-ink-2">{{ issue.hint }}</div>
          </div>
        </div>
        <div v-if="health.score !== null" class="mb-1 text-ink-2">Health {{ health.score }}/100</div>

        <template v-for="section in panelModel.sections" :key="section.id">
          <h3 class="mt-1.5 text-2xs font-semibold tracking-wider uppercase" :class="COLOR_TEXT[section.color]">
            <button type="button" class="flex w-full items-center gap-1 uppercase hover:brightness-125 focus-visible:outline-2 focus-visible:outline-focus" :aria-expanded="!isFolded(section.id as SectionId)" @click="toggleSection(section.id as SectionId)">
              <Icon name="section-chevron" class="size-2.5 transition-transform" :class="isFolded(section.id as SectionId) && '-rotate-90'" />
              {{ section.heading }}
              <span v-if="isFolded(section.id as SectionId)" class="ml-auto min-w-0 truncate text-right font-normal tracking-normal normal-case" :class="TONE_TEXT[section.summary.tone] || 'text-ink'">{{ section.summary.text }}</span>
            </button>
          </h3>
          <dl v-show="!isFolded(section.id as SectionId)" class="grid grid-cols-[1fr_auto] [&>dt]:flex [&>dt]:items-center [&>dt]:text-ink-2 [&>dt]:whitespace-nowrap [&>dt]:after:ml-1.5 [&>dt]:after:h-px [&>dt]:after:flex-1 [&>dt]:after:bg-ink-2/50 [&>dd]:flex [&>dd]:items-center [&>dd]:justify-end [&>dd]:whitespace-pre [&>dd]:text-ink [&>dd]:before:mr-1.5 [&>dd]:before:h-px [&>dd]:before:flex-1 [&>dd]:before:bg-ink-2/50">
            <template v-for="row in section.rows" :key="row.id">
              <dt :title="row.tooltip">{{ row.label }}</dt>
              <dd :class="TONE_TEXT[row.tone] || undefined"><template v-if="row.segments.length === 1">{{ row.segments[0]!.text }}</template><template v-else><template v-for="(seg, i) in row.segments" :key="i"><span v-if="seg.tone !== 'none'" :class="TONE_TEXT[seg.tone]">{{ seg.text }}</span><template v-else>{{ seg.text }}</template></template></template></dd>
            </template>
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
