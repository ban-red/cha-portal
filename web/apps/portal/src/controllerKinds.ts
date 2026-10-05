import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { computed, reactive } from "vue";

import { ApiError, api, type ControllerApp, type PadKind } from "./api";
import { notAvailable, patchApp } from "./storage";

// Which virtual controller each app sees, whatever controller is in your hands.
// The choice is made when an app starts, so it applies from its next launch.
// Shared by the Controllers page and the app cards.

export const CONTROLLER_APPS_KEY = ["controller-apps"] as const;

export const KINDS: { kind: PadKind; label: string; about: string }[] = [
  {
    kind: "xbox360",
    label: "Xbox 360",
    about: "Buttons, sticks, triggers and rumble. Every game knows it.",
  },
  {
    kind: "dualsense",
    label: "DualSense",
    about:
      "A PlayStation 5 controller, with its touchpad, motion sensors, lightbar, adaptive triggers and rumble. Games that support it use them.",
  },
  {
    kind: "steam",
    label: "Steam Controller",
    about: "A Steam Controller for Steam Input: both trackpads, motion sensors, back grips and haptics.",
  },
];

export const kindLabel = (kind: PadKind) => KINDS.find((k) => k.kind === kind)?.label ?? kind;

export function useControllerApps() {
  const queryClient = useQueryClient();
  const query = useQuery({ queryKey: CONTROLLER_APPS_KEY, queryFn: api.controllerApps, staleTime: 30_000 });
  const apps = computed(() => query.data.value?.apps ?? []);
  const byTemplate = computed(() => new Map(apps.value.map((a) => [a.template, a])));
  // An older server has no such call: callers leave the choice out.
  const missing = computed(() => notAvailable(query.error.value));
  const errors = reactive<Record<string, string>>({});

  const save = useMutation({
    mutationFn: (v: { app: ControllerApp; kind: PadKind | null }) => api.setControllerKind(v.app.template, v.kind),
    onMutate: async ({ app, kind }) => {
      delete errors[app.template];
      await queryClient.cancelQueries({ queryKey: CONTROLLER_APPS_KEY });
      patchApp<ControllerApp>(queryClient, CONTROLLER_APPS_KEY, app.template, { kind });
      return { was: app.kind };
    },
    onError: (err, { app }, ctx) => {
      patchApp<ControllerApp>(queryClient, CONTROLLER_APPS_KEY, app.template, { kind: ctx?.was ?? null });
      errors[app.template] =
        err instanceof ApiError && err.message ? `Couldn't save the change: ${err.message}` : "Couldn't save the change.";
    },
    onSettled: () => void queryClient.invalidateQueries({ queryKey: CONTROLLER_APPS_KEY }),
  });

  /** From a `<select>`'s change: "" is the app's default. */
  function choose(app: ControllerApp, event: Event) {
    const value = (event.target as HTMLSelectElement).value;
    save.mutate({ app, kind: value === "" ? null : (value as PadKind) });
  }

  return { query, apps, byTemplate, missing, errors, choose };
}
