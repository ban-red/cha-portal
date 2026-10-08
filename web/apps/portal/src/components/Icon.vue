<script setup lang="ts">
// A spec icon (@cha/ui-spec, icons.json) as inline SVG. Size it with a class: <Icon name="copy" class="size-3" />.
import { computed } from "vue";
import { ICONS, type IconName } from "@cha/ui-spec";

const props = defineProps<{ name: IconName }>();
const icon = computed(() => ICONS[props.name]!);
</script>

<template>
  <svg
    :viewBox="`0 0 ${icon.viewBox} ${icon.viewBox}`"
    fill="none"
    stroke="currentColor"
    :stroke-linecap="icon.linecap"
    :stroke-linejoin="icon.linejoin"
    aria-hidden="true"
  >
    <path
      v-for="(part, i) in icon.parts"
      :key="i"
      :d="part.d"
      :stroke-width="part.stroke"
      :fill="part.fill ? 'currentColor' : undefined"
      :stroke="part.fill ? 'none' : undefined"
    />
  </svg>
</template>
