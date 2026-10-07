<script setup lang="ts">
import { Check, Laptop, X } from "lucide-vue-next";
import { computed, ref } from "vue";
import { useRoute } from "vue-router";

import { ApiError, api, type DeviceCodeInfo } from "../api";
import FormError from "../components/FormError.vue";
import { normalizeUserCode, typedUserCode } from "../player";

// Cha Player shows a code and "<portal>/link"; the user types it here to sign the player in.
const route = useRoute();

const typed = ref(typedUserCode(typeof route.query.code === "string" ? route.query.code : ""));
const code = computed(() => normalizeUserCode(typed.value));

const device = ref<DeviceCodeInfo | null>(null);
/** The code the device above was looked up with. */
const looked = ref<string | null>(null);
const done = ref<"approved" | "denied" | null>(null);
const busy = ref(false);
const error = ref<string | null>(null);

function failText(err: unknown): string {
  if (err instanceof ApiError && err.status === 404) {
    return "That code isn't valid, or it has expired. Check the code shown in Cha Player, or start again there.";
  }
  if (err instanceof ApiError && err.message) return err.message;
  return "Couldn't reach the portal. Try again.";
}

function onInput(value: string) {
  typed.value = typedUserCode(value);
  error.value = null;
}

async function look() {
  if (busy.value) return;
  if (!code.value) {
    error.value = "Type the 8 letters Cha Player shows, like ABCD-EFGH.";
    return;
  }
  busy.value = true;
  error.value = null;
  try {
    device.value = await api.deviceCode(code.value);
    looked.value = code.value;
  } catch (err) {
    device.value = null;
    error.value = failText(err);
  } finally {
    busy.value = false;
  }
}

async function decide(approve: boolean) {
  if (busy.value || !looked.value) return;
  busy.value = true;
  error.value = null;
  try {
    await (approve ? api.approveDeviceCode(looked.value) : api.denyDeviceCode(looked.value));
    done.value = approve ? "approved" : "denied";
  } catch (err) {
    error.value = failText(err);
    if (err instanceof ApiError && err.status === 404) device.value = null;
  } finally {
    busy.value = false;
  }
}

function again() {
  typed.value = "";
  device.value = null;
  looked.value = null;
  done.value = null;
  error.value = null;
}
</script>

<template>
  <div class="max-w-lg space-y-6">
    <section class="card space-y-4 px-4 py-4 sm:px-5" aria-labelledby="link-title">
      <h2 id="link-title" class="text-base font-semibold">Sign in Cha Player</h2>

      <div v-if="done" class="space-y-3" aria-live="polite">
        <p v-if="done === 'approved'" class="flex items-start gap-2 text-sm text-ok">
          <Check class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
          <span>Approved. {{ device?.name }} is signed in; you can go back to Cha Player.</span>
        </p>
        <p v-else class="text-sm text-ink-2">Denied. {{ device?.name }} was not signed in.</p>
        <button type="button" class="btn-ghost px-3 py-1.5" @click="again">Link another device</button>
      </div>

      <form v-else-if="!device" class="space-y-3" @submit.prevent="look">
        <p class="text-sm text-ink-2">Type the code Cha Player shows.</p>
        <div class="flex flex-wrap items-center gap-2">
          <label for="user-code" class="sr-only">Code from Cha Player</label>
          <input
            id="user-code"
            :value="typed"
            class="field w-44 text-center font-mono tracking-widest uppercase"
            autocomplete="off"
            autocapitalize="characters"
            spellcheck="false"
            maxlength="9"
            placeholder="ABCD-EFGH"
            :disabled="busy"
            autofocus
            @input="onInput(($event.target as HTMLInputElement).value)"
          />
          <button type="submit" class="btn-primary px-4 py-2" :disabled="busy || !code">
            {{ busy ? "Checking…" : "Continue" }}
          </button>
        </div>
        <FormError polite :message="error" />
      </form>

      <div v-else class="space-y-4">
        <div class="flex items-center gap-3 rounded-lg border border-line bg-panel-2 p-3">
          <Laptop class="size-6 shrink-0 text-accent" aria-hidden="true" />
          <div class="min-w-0">
            <p class="truncate text-sm font-medium">{{ device.name }}</p>
            <p class="text-xs text-ink-3">
              wants to sign in to this portal as you · code <span class="font-mono">{{ looked }}</span>
            </p>
          </div>
        </div>
        <p class="text-sm text-ink-2">Approve only if you just started this in Cha Player on your own device.</p>
        <div class="flex flex-wrap gap-2">
          <button type="button" class="btn-primary px-4 py-2" :disabled="busy" @click="decide(true)">
            <Check class="size-4" aria-hidden="true" />
            {{ busy ? "Working…" : "Approve" }}
          </button>
          <button type="button" class="btn-ghost px-4 py-2" :disabled="busy" @click="decide(false)">
            <X class="size-4" aria-hidden="true" />
            Deny
          </button>
        </div>
        <FormError polite :message="error" />
      </div>
    </section>
  </div>
</template>
