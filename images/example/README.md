# Example: a third-party environment image

The smallest image that runs on a Cha node: the [foot](https://codeberg.org/dnkl/foot) terminal, a native Wayland client from Ubuntu's archive, on Cha's base image. It is 14 MB on top of the base. The rules it follows are in [`docs/image-spec.md`](../../docs/image-spec.md).

| File | What |
|---|---|
| `Dockerfile` | `FROM` the base (pinned by version), installs foot, seeds the home, sets `CMD` |
| `foot.ini` | Copied to `/home/cha/.config/foot/`. The node copies the image's home to a user's directory once, the first time they keep their data |
| `catalog.json` | The catalog entry an admin would load for it |

## Build

```bash
docker build -t ghcr.io/your-name/cha-env-foot:0.2.1 images/example
```

`0.2.1` is the node agent release you target (`{version}` in the catalog). To build on a local base (`docker compose -f images/compose.yaml build` makes `cha/env-base:dev`):

```bash
docker build --build-arg BASE=cha/env-base:dev -t cha/env-example:dev images/example
```

Checked on a node by building it this way (416 MB against the base's 402 MB) and running `foot --version` and `id -u` in it. It has not been launched through a portal.

## Check and publish

```bash
cha-node --check-image ghcr.io/your-name/cha-env-foot:0.2.1 --profile standard
docker push ghcr.io/your-name/cha-env-foot:0.2.1
```

Publish one tag per node agent release you support. The catalog's `image` is `ghcr.io/your-name/cha-env-foot:{version}`, and the registry must be public, since the node pulls without credentials.

## What the catalog entry says

- `image` names a registry, as an external catalog requires, and has no `localImage`.
- `security` is `standard`: foot creates no namespaces.
- `class` is `desktop`; `shmMb` is 128 (foot needs little).
- No `persistent`, so users start with a throwaway home. Set `"persistent": true` to let them keep `~/.config/foot` and their shell history.

Validate it against the schema:

```bash
bunx ajv-cli@5 validate --spec=draft2020 -s images/catalog.schema.json -d images/example/catalog.json
```
