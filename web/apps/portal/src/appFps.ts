import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { computed, reactive } from "vue";

import { ApiError, api, type AppSettings, type Fps } from "./api";
import { notAvailable, patchApp } from "./storage";

// The frame rate each app runs at. The choice is made when an app starts, so
// it applies from its next launch. Used by the app cards.

export const APP_SETTINGS_KEY = ["app-settings"] as const;

export const FPS_CHOICES: Fps[] = [60, 90, 120];

export function useAppFps() {
  const queryClient = useQueryClient();
  const query = useQuery({ queryKey: APP_SETTINGS_KEY, queryFn: api.appSettings, staleTime: 30_000 });
  const apps = computed(() => query.data.value?.apps ?? []);
  const byTemplate = computed(() => new Map(apps.value.map((a) => [a.template, a])));
  // An older server has no such call: callers leave the choice out.
  const missing = computed(() => notAvailable(query.error.value));
  const errors = reactive<Record<string, string>>({});

  const save = useMutation({
    mutationFn: (v: { app: AppSettings; fps: Fps | null }) => api.setAppFps(v.app.template, v.fps),
    onMutate: async ({ app, fps }) => {
      delete errors[app.template];
      await queryClient.cancelQueries({ queryKey: APP_SETTINGS_KEY });
      patchApp<AppSettings>(queryClient, APP_SETTINGS_KEY, app.template, { fps });
      return { was: app.fps };
    },
    onError: (err, { app }, ctx) => {
      patchApp<AppSettings>(queryClient, APP_SETTINGS_KEY, app.template, { fps: ctx?.was ?? null });
      errors[app.template] =
        err instanceof ApiError && err.message ? `Couldn't save the change: ${err.message}` : "Couldn't save the change.";
    },
    onSettled: () => void queryClient.invalidateQueries({ queryKey: APP_SETTINGS_KEY }),
  });

  /** From a `<select>`'s change: "" is the app's default. */
  function choose(app: AppSettings, event: Event) {
    const value = (event.target as HTMLSelectElement).value;
    save.mutate({ app, fps: value === "" ? null : (Number(value) as Fps) });
  }

  return { query, apps, byTemplate, missing, errors, choose };
}
