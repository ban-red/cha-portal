// The Controllers page's notice on a Steam Controller that only the Gamepad API shows.
import { afterAll, expect, test } from "bun:test";

import vue from "@vitejs/plugin-vue";
import { createSSRApp, h } from "vue";
import { renderToString } from "vue/server-renderer";
import { createServer } from "vite";

const server = await createServer({ configFile: false, plugins: [vue()], server: { middlewareMode: true }, appType: "custom", logLevel: "error", root: `${import.meta.dir}/..` });
const Note = (await server.ssrLoadModule("/src/components/SteamWebHidNote.vue")).default;
afterAll(() => server.close());

const render = (props: Record<string, unknown>) => renderToString(createSSRApp({ render: () => h(Note, props) }));

test("with WebHID: what is lost, and a button to connect", async () => {
  const out = await render({ hidReason: null, connecting: false });
  expect(out).toContain("Chrome is reading this controller as a basic gamepad");
  expect(out).toContain("Steam button, trackpads and gyro don&#39;t come through");
  expect(out).toContain("Connect with WebHID");
  expect(out).not.toContain("disabled");
});

test("while the picker is open the button is off", async () => {
  expect(await render({ hidReason: null, connecting: true })).toContain("disabled");
});

test("without WebHID: one line, no button", async () => {
  const out = await render({ hidReason: "This browser has no WebHID.", connecting: false });
  expect(out).toContain("Chrome or Edge can use all of it");
  expect(out).not.toContain("<button");
});
