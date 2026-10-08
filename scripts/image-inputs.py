#!/usr/bin/env python3
"""A hash of everything each published image is built from.

The publish workflow tags every image it builds with `:inputs-<hash>`. On the
next run, an image whose hash already exists in the registry is not rebuilt:
the existing image gets the new tags instead. So a release rebuilds only the
images whose inputs changed.

An image's inputs are its Dockerfile (and, for an environment, its folder
under images/), the workspace's Cargo.toml, Cargo.lock and .dockerignore, every
file of the crates it builds and of their path dependencies (from
`cargo metadata`), and every file those crates compile in with
`include_str!`, `include_bytes!` or `include!`. The portal adds the web
workspace. An environment adds its base's hash. Files that never reach an
image are left out: Markdown, crates' tests/, benches/ and examples/ (a
release build compiles none of them), and the portal's and player's
*.test.ts and the Steam image's tests. The hash is over the files' git
blob ids, so it needs the files committed or staged.

Not covered: the upstream images and packages a Dockerfile pulls (ubuntu:26.04,
Chrome, Steam, apt). Bump REBUILD_EPOCH to rebuild everything, or run the
workflow by hand with "rebuild" ticked.

    scripts/image-inputs.py                  # every image, one per line
    scripts/image-inputs.py cha-node         # one image
    scripts/image-inputs.py --json           # {"cha-node": "…", …} for CI
    scripts/image-inputs.py --files cha-node # the files that go into it
"""

import hashlib
import json
import os
import re
import subprocess
import sys

# Bump to rebuild every image on the next run.
# 2: the environments (and their base) are now pushed with zstd layers.
REBUILD_EPOCH = 2

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RUST = ["Cargo.toml", "Cargo.lock", ".dockerignore"]
WEB = ["web", "package.json", "bun.lock"]

# name: (paths, crates, base image or None)
IMAGES = {
    "cha-portal": (["deploy/portal/Dockerfile", *WEB], ["cha-control"], None),
    "cha-node": (["deploy/node/Dockerfile"], ["cha-node"], None),
    "cha-streamer": (["deploy/streamer/Dockerfile"], ["cha-streamer"], None),
    "cha-gateway": (["deploy/gateway/Dockerfile"], ["cha-gateway"], None),
    "cha-env-base": (["images/base"], [], None),
    "cha-env-test-pattern": (["images/test-pattern"], ["cha-testpattern"], "cha-env-base"),
    "cha-env-chrome": (["images/chrome"], [], "cha-env-base"),
    "cha-env-firefox": (["images/firefox"], [], "cha-env-base"),
    "cha-env-xfce": (["images/xfce"], ["cha-x11-clipboard"], "cha-env-base"),
    "cha-env-kde": (["images/kde"], [], "cha-env-base"),
    "cha-env-steam": (["images/steam"], ["cha-x11-clipboard"], "cha-env-base"),
}

# Files that never reach an image: docs, and tests a release build doesn't
# compile or run (`cargo build --release -p <crate>` builds no tests, benches
# or examples; the portal's typecheck and bundle exclude *.test.ts).
IGNORED = re.compile(
    r"\.md$"
    r"|(^|/)test_[^/]*\.py$"
    r"|\.test\.ts$"
    r"|^crates/[^/]+/(tests|benches|examples)/"
    # The Steam image runs `make`, not `make tests`.
    r"|^images/steam/uinput-shim/(test\.c|run-tests\.sh)$"
)
INCLUDE = re.compile(r'\binclude(?:_str|_bytes)?!\(\s*"([^"]+)"\s*\)')


def git(*args):
    return subprocess.run(["git", *args], cwd=ROOT, check=True, capture_output=True, text=True).stdout


def crate_dirs(roots):
    """The directories of `roots` and of every path dependency they build with."""
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"],
            cwd=ROOT, check=True, capture_output=True, text=True,
        ).stdout
    )
    packages = {p["name"]: p for p in meta["packages"]}
    seen, todo = set(), list(roots)
    while todo:
        name = todo.pop()
        if name in seen:
            continue
        seen.add(name)
        for dep in packages[name]["dependencies"]:
            # Dev-dependencies don't reach the binary; optional ones may.
            if dep.get("path") and dep.get("kind") != "dev":
                todo.append(dep["name"])
    return sorted(
        os.path.relpath(os.path.dirname(packages[n]["manifest_path"]), ROOT) for n in seen
    )


def included_files(dirs):
    """Files outside `dirs` that their Rust sources compile in."""
    found = set()
    for path in git("ls-files", "--", *dirs).splitlines():
        if not path.endswith(".rs") or IGNORED.search(path):
            continue
        with open(os.path.join(ROOT, path), encoding="utf-8") as f:
            for rel in INCLUDE.findall(f.read()):
                target = os.path.normpath(os.path.join(os.path.dirname(path), rel))
                if not target.startswith(".."):
                    found.add(target)
    return found


def files(name):
    paths, crates, _ = IMAGES[name]
    inputs = list(paths)
    if crates:
        dirs = crate_dirs(crates)
        inputs += RUST + dirs
        inputs += sorted(included_files(dirs))
    listed = git("ls-files", "-s", "--", *inputs).splitlines()
    # "<mode> <blob> <stage>\t<path>"
    kept = [line for line in listed if not IGNORED.search(line.split("\t", 1)[1])]
    return sorted(kept, key=lambda line: line.split("\t", 1)[1])


def inputs_hash(name, memo={}):
    if name not in memo:
        h = hashlib.sha256()
        h.update(f"epoch {REBUILD_EPOCH}\nimage {name}\n".encode())
        base = IMAGES[name][2]
        if base:
            h.update(f"base {inputs_hash(base)}\n".encode())
        for line in files(name):
            h.update(line.encode() + b"\n")
        memo[name] = h.hexdigest()[:20]
    return memo[name]


def main(argv):
    if argv[:1] == ["--files"]:
        for name in argv[1:]:
            for line in files(name):
                print(line.split("\t", 1)[1])
        return 0
    if argv == ["--json"]:
        print(json.dumps({n: inputs_hash(n) for n in IMAGES}))
        return 0
    for name in argv or IMAGES:
        if name not in IMAGES:
            print(f"unknown image: {name}", file=sys.stderr)
            return 2
        print(name, inputs_hash(name))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
