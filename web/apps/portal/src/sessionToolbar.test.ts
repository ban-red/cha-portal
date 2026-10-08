// The toolbar renderer draws what the spec's model has: every control and every row of every menu
// shows up in the markup. (The spec check covers that each is in toolbar.json; this covers that the
// Vue side knows how to draw it.) SessionToolbar.vue is loaded through Vite, as the app is.
import { afterAll, describe, expect, test } from "bun:test";

import vue from "@vitejs/plugin-vue";
import { buildToolbar, TOOLBAR, type ToolbarModel } from "@cha/ui-spec";
import { capsOf, stateOf, TOOLBAR_CASES } from "@cha/ui-spec/toolbar-cases";
import { createSSRApp, h } from "vue";
import { renderToString } from "vue/server-renderer";
import { createMemoryHistory, createRouter } from "vue-router";
import { createServer } from "vite";

const server = await createServer({ configFile: false, plugins: [vue()], server: { middlewareMode: true }, appType: "custom", logLevel: "error", root: `${import.meta.dir}/..` });
const SessionToolbar = (await server.ssrLoadModule("/src/components/SessionToolbar.vue")).default;
afterAll(() => server.close());

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");

async function html(model: ToolbarModel): Promise<string> {
  const router = createRouter({ history: createMemoryHistory(), routes: ["/", "/controllers"].map((path) => ({ path, component: { render: () => h("div") } })) });
  await router.push("/");
  const app = createSSRApp({ render: () => h(SessionToolbar, { model, gpu: { kind: "nvidia", name: "RTX" } }) });
  app.use(router);
  return renderToString(app);
}

const base = TOOLBAR_CASES.find((c) => c.name === "web: a full portal session shows every control")!;
const full = (menu: string) => buildToolbar(TOOLBAR, { ...stateOf(base, "web"), menu_open: menu, hid_reason: "No WebHID.", note: "Nothing added.", controllers: [{ name: "Pad", slot: 0 }, { name: "Stick", slot: null }] }, capsOf(base), "web");

describe("SessionToolbar draws the model", () => {
  for (const menu of ["", "stream", "sound", "controllers"]) {
    test(`everything in the model shows (menu: ${menu || "none"})`, async () => {
      const model = full(menu);
      const out = await html(model);
      const missing: string[] = [];
      const seen = (what: string, text: string | null) => {
        if (text && !out.includes(esc(text))) missing.push(`${what}: ${text}`);
      };
      for (const c of model.controls) {
        if (c.id === "gpu") continue; // GpuBadge draws its own words
        seen(`${c.id} tooltip`, c.tooltip);
        seen(`${c.id} aria-label`, c.aria_label);
        seen(`${c.id} label`, c.label);
        seen(`${c.id} badge`, c.badge?.text ?? null);
        for (const i of c.items ?? []) {
          seen(`${c.id} item`, i.text);
          seen(`${c.id} item action`, i.action?.label ?? null);
        }
        for (const r of c.menu?.rows ?? []) {
          seen(`${r.id} label`, r.label);
          seen(`${r.id} text`, r.text);
          seen(`${r.id} tooltip`, r.tooltip);
          for (const o of r.options ?? []) seen(`${r.id} option`, o.label);
          for (const i of r.items ?? []) {
            seen(`${r.id} item`, i.text);
            seen(`${r.id} item detail`, i.detail);
          }
          if (r.slider) seen(`${r.id} slider`, r.slider.display);
        }
      }
      expect(missing).toEqual([]);
      if (menu) expect(model.controls.find((c) => c.id === menu)?.menu?.rows.length).toBeGreaterThan(0);
    });
  }
  test("the folded bar and the hide tab say what the spec says", async () => {
    const out = await html(full(""));
    expect(out).toContain(esc(TOOLBAR.folded.bar.tooltip));
    expect(out).toContain(esc(TOOLBAR.folded.tab.tooltip));
  });
});
