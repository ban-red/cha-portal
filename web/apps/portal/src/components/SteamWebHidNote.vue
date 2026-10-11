<script setup lang="ts">
import { TriangleAlert } from "lucide-vue-next";

// On a Steam Controller's card when only the Gamepad API shows it. There the Steam button arrives
// as A and the gyro and trackpads are hidden; WebHID reads the controller itself. Where the browser
// has no WebHID (`hidReason` is why) it says so instead of offering a button that can't work.
defineProps<{ hidReason: string | null; connecting: boolean }>();
defineEmits<{ connect: [] }>();
</script>

<template>
  <div role="status" class="mt-3 flex flex-wrap items-start justify-between gap-x-4 gap-y-2 rounded-lg border border-warn/30 bg-warn/10 px-3 py-2 text-sm text-warn">
    <p class="flex min-w-0 flex-1 items-start gap-2">
      <TriangleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
      <span class="min-w-0 break-words">
        <template v-if="hidReason">
          This browser is reading this controller as a basic gamepad: the Steam button, trackpads and gyro don't come
          through. Chrome or Edge can use all of it.
        </template>
        <template v-else>
          Chrome is reading this controller as a basic gamepad: the Steam button, trackpads and gyro don't come
          through. Connect it here to use all of it.
        </template>
      </span>
    </p>
    <button v-if="!hidReason" type="button" class="btn-primary shrink-0 px-3 py-1.5 text-xs" :disabled="connecting" @click="$emit('connect')">
      {{ connecting ? "Choosing…" : "Connect with WebHID" }}
    </button>
  </div>
</template>
