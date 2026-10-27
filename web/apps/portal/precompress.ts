// After a production build, writes a Brotli (`.br`, the strongest setting) and
// a gzip (`.gz`) copy beside each text file in the output, when it saves
// anything. cha-control serves whichever the browser accepts in place of the
// file (ServeDir's precompressed_*), so the app goes out at its smallest and
// the portal never compresses per request. A reverse proxy that compresses
// (deploy/portal/Caddyfile) passes an encoded response through as it is.

import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { brotliCompressSync, constants, gzipSync } from "node:zlib";
import type { Plugin } from "vite";

const TEXT = /\.(?:js|mjs|css|html|svg|json|txt|webmanifest|wasm)$/;
/** Below this, the encoding's framing and the extra request headers win nothing. */
const MIN_BYTES = 1024;

function* files(dir: string): Generator<string> {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) yield* files(path);
    else if (entry.isFile()) yield path;
  }
}

export function precompress(): Plugin {
  let outDir = "";
  return {
    name: "cha-precompress",
    apply: "build",
    configResolved(config) {
      outDir = resolve(config.root, config.build.outDir);
    },
    closeBundle() {
      for (const path of files(outDir)) {
        if (!TEXT.test(path)) continue;
        const data = readFileSync(path);
        if (data.length < MIN_BYTES) continue;
        const br = brotliCompressSync(data, {
          params: {
            [constants.BROTLI_PARAM_QUALITY]: constants.BROTLI_MAX_QUALITY,
            [constants.BROTLI_PARAM_SIZE_HINT]: data.length,
          },
        });
        if (br.length < data.length) writeFileSync(`${path}.br`, br);
        const gz = gzipSync(data, { level: 9 });
        if (gz.length < data.length) writeFileSync(`${path}.gz`, gz);
      }
    },
  };
}
