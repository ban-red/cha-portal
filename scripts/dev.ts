#!/usr/bin/env bun
// A local dev instance of the portal, from the repository root:
//
//   bun run dev                         # cha-control + the SPA with hot reload
//   bun run dev -- --turn-secret …      # extra flags go to cha-control
//
// - cha-control on 0.0.0.0:7677 (`cargo run`), database `data/dev.db`,
//   with `--dev-login`: the sign-in page offers "Login as Local Dev", and a
//   button for each existing admin account (to see your own data locally).
// - The SPA on Vite at http://localhost:7678 (PORT to change), hot reloading,
//   proxying /api to cha-control (vite.config.ts). It listens on 0.0.0.0 too
//   (CHA_WEB_HOST=127.0.0.1 keeps it to this machine).
//
// It listens on every interface, so a node on another machine can enroll
// (CHA_LISTEN=127.0.0.1:7677 keeps it to this machine). A node agent on this
// machine uses http://127.0.0.1:7677; one elsewhere runs with
// CHA_PORTAL_URL=http://<this machine>:7677 and
// CHA_ALLOW_INSECURE_PORTAL=true (plain HTTP). "Login as Local Dev" still only
// works from this machine (cha-control checks the peer address; the Vite proxy
// stays on localhost). Ctrl-C stops both; if either exits, the other is
// stopped too. Vite starts after cha-control is answering, so the first page
// load doesn't hit a dead proxy.

import { mkdirSync } from "node:fs";
import { hostname } from "node:os";
import { join } from "node:path";

const root = join(import.meta.dir, "..");
const LISTEN = process.env.CHA_LISTEN ?? "0.0.0.0:7677";
const CONTROL_PORT = LISTEN.slice(LISTEN.lastIndexOf(":") + 1);
// This machine's own way in, whatever LISTEN is bound to.
const CONTROL = `127.0.0.1:${CONTROL_PORT}`;
const LAN = !/^(127\.|localhost:|\[::1\]:)/.test(LISTEN);
const WEB_PORT = process.env.PORT ?? "7678";
const WEB_HOST = process.env.CHA_WEB_HOST ?? "0.0.0.0";

mkdirSync(join(root, "data"), { recursive: true });

type Child = { name: string; proc: Bun.Subprocess<"ignore", "pipe", "pipe"> };

function start(name: string, color: number, cmd: string[], env: Record<string, string> = {}): Child {
  const proc = Bun.spawn(cmd, {
    cwd: root,
    env: { ...process.env, FORCE_COLOR: "1", ...env },
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
  });
  const tag = `\x1b[${color}m${name.padEnd(7)}\x1b[0m│ `;
  for (const stream of [proc.stdout, proc.stderr]) void relay(stream, tag);
  return { name, proc };
}

/** Copies a child's output line by line, tagged with its name. */
async function relay(stream: ReadableStream<Uint8Array>, tag: string): Promise<void> {
  const decoder = new TextDecoder();
  let rest = "";
  for await (const chunk of stream) {
    const lines = (rest + decoder.decode(chunk, { stream: true })).split("\n");
    rest = lines.pop() ?? "";
    for (const line of lines) process.stdout.write(tag + line + "\n");
  }
  if (rest) process.stdout.write(tag + rest + "\n");
}

const children: Child[] = [
  start("control", 36, [
    "cargo",
    "run",
    "-p",
    "cha-control",
    "--",
    "--listen",
    LISTEN,
    "--database",
    "data/dev.db",
    "--dev-login",
    ...process.argv.slice(2),
  ]),
];

let stopping = false;
function stop(code: number): void {
  if (stopping) return;
  stopping = true;
  for (const { proc } of children) if (proc.exitCode === null) proc.kill("SIGTERM");
  // Give them a moment to exit cleanly, then make sure.
  setTimeout(() => {
    for (const { proc } of children) if (proc.exitCode === null) proc.kill("SIGKILL");
    process.exit(code);
  }, 3000).unref();
  void Promise.all(children.map((c) => c.proc.exited)).then(() => process.exit(code));
}

process.on("SIGINT", () => stop(0));
process.on("SIGTERM", () => stop(0));

/** Stops the whole dev instance when this child exits on its own. */
function watch({ name, proc }: Child): void {
  void proc.exited.then((code) => {
    if (stopping) return;
    console.error(`\n${name} exited (${code}); stopping the dev instance.`);
    stop(code || 1);
  });
}
watch(children[0]!);

// Vite starts only once cha-control answers (the first `cargo run` compiles),
// so the page never loads against a proxy with nothing behind it.
const startedAt = Date.now();
while (!stopping) {
  const up = await fetch(`http://${CONTROL}/api/health`)
    .then((r) => r.ok)
    .catch(() => false);
  if (up) break;
  await Bun.sleep(250);
}

if (!stopping) {
  const web = start("web", 35, ["bun", "run", "--cwd", "web/apps/portal", "dev", "--host", WEB_HOST, "--port", WEB_PORT, "--strictPort"], {
    CHA_CONTROL: `http://${CONTROL}`,
  });
  children.push(web);
  watch(web);

  const secs = ((Date.now() - startedAt) / 1000).toFixed(0);
  console.log(
    `\n\x1b[1mPortal dev instance ready\x1b[0m (${secs} s): http://localhost:${WEB_PORT} ` +
      `(hot reload; "Login as Local Dev" signs you in). API: http://${CONTROL}\n`,
  );
  if (LAN) {
    const host = hostname().replace(/\.local$/, "");
    console.log(
      `Listening on the LAN (${LISTEN}). Nodes elsewhere: CHA_PORTAL_URL=http://${host}.lan:${CONTROL_PORT} ` +
        `CHA_ALLOW_INSECURE_PORTAL=true (use whatever name they resolve this machine by)\n`,
    );
  }
}
