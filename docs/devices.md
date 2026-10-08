# Devices

What an environment runs on. A node offers one or more **devices**; each
launch runs on one of them, chosen by the user from the app card's launch menu
or, by default, the best one the portal can find.

| Kind | Composites on | Encodes with | Codecs | Notes |
|---|---|---|---|---|
| `nvidia` | the NVIDIA GPU (EGL on its render node) | NVENC (CUDA) | H.264, HEVC, AV1, PyroWave | Today's only path. The app gets the GPU through CDI. |
| `vaapi` | an Intel or AMD GPU (EGL on its render node, Mesa) | VA-API (libva: Intel's iHD, Mesa's radeonsi) | H.264 (what the device's encode entrypoints offer, of what the streamer has written: HEVC and AV1 follow) | The app gets that render node. Intel iGPUs (QuickSync), Arc, AMD. The encoder is written but not yet run on a GPU. |
| `cpu` | Mesa's llvmpipe (software EGL) | x264 and SVT-AV1, in software | H.264, AV1 | No GPU for the streamer or the app. Desktops and browsers at modest sizes; never 3D apps or games. |

## Inventory

`Inventory.devices` (cha-wire, optional; absent = an older node, read as one
`nvidia` device when its `gpus` list an NVIDIA GPU with encoders):

```
devices: [{ id, kind: "nvidia"|"vaapi"|"cpu", name,
            renderNode?: "/dev/dri/renderD129", vendor?: "intel"|"amd"|"nvidia",
            codecs: ["h264","hevc","av1","pyrowave420",…], cores? }]
```

- `id` is stable on the node: `nvidia:<index>`, `vaapi:renderD<N>`, `cpu`.
- The node finds VA-API devices among `/dev/dri/renderD*` whose driver isn't
  `nvidia`, and asks the streamer image what each can encode
  (`cha-streamer --probe-device vaapi:/dev/dri/renderD129`, in a throwaway
  container given that node, printing JSON), at startup and on `--doctor`.
- `cpu` is always there, with the machine's core count. Its codecs are what
  the streamer image says (`cha-streamer --probe-device cpu`, the same
  throwaway container with no device, at startup and on `--doctor`): `h264`,
  and `av1` when the image has SVT-AV1. The node offers `h264` alone until
  the image answers, or if it doesn't (an older image).
- At most one `nvidia` device per node: the first GPU with encoders (CDI
  gives environments every GPU; the streamer composites on the configured
  render node).
- The probe container has no network, a read-only root, no capabilities and
  only that render node; the node reads the last stdout line starting with
  `{`, and a non-zero exit (an older image) means no VA-API device.

## Launching

- `EnvironmentSpec.device` (optional `{ id, kind, renderNode? }`; absent =
  `nvidia`, as before). The node gives the streamer `--device vaapi
  --render-node <node>` or `--device cpu` (no render node); for `nvidia` it
  passes no `--device`, so older streamer images still start. The app gets
  what that kind needs: the CDI GPU
  for `nvidia`, that render node (and its group) for `vaapi`, nothing for
  `cpu`.
- Catalog: `needsGpu: true` for apps that need real 3D (Steam, games): they
  never run on `cpu`.

## Choosing

- `GET /api/placements?template=<id>` → `{ auto: {node, device} | null,
  options: [{ node, nodeName, device, kind, label, allowed, reason?, score }] }`
  over online nodes; `node` and `device` are ids (`options[].device` the
  device's id). Without `template`: `{ templates: { <id>: { auto, options } } }`,
  which the app cards use.
- `POST /api/environments` takes optional `node` and `device`; absent = the
  `auto` choice. A choice that isn't allowed is refused (400 with the reason).
- Score: by kind (`nvidia` > `vaapi` > `cpu`), less for load from the node's
  live usage (CPU, the device's GPU use, free VRAM; a GPU with under ~2 GB of
  VRAM free isn't chosen automatically), less per environment already
  running there. Not allowed: `cpu` for `needsGpu` apps; a device without a
  codec the browser can decode; an offline node.
- The app card's Launch becomes a split button: "Launch" (the auto choice,
  named under it, e.g. "on gpu-node · RTX 4090") and a menu of every option,
  the disallowed ones disabled with their reason.
