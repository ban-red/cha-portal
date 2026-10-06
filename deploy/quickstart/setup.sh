#!/bin/sh
# Prepares one Ubuntu or Debian machine to run the quick start
# (deploy/quickstart/compose.yaml): the portal and a node side by side.
# Run it with sudo from the repository, checked out at the release you'll run:
#   git checkout v0.1.0
#   sudo deploy/quickstart/setup.sh --version 0.1.0 --tailscale
#   deploy/quickstart/setup.sh --check      # only reports; needs no root
#
# Each step does nothing when it's already done, so it is safe to run again,
# and after an update:
# 1. packages it needs (curl, git, AppArmor's tools, kmod, udev), and the
#    clock kept in sync (media tokens last 60 s);
# 2. Docker Engine 26 or newer with the compose and buildx plugins, from
#    Docker's own repository; it won't replace a distribution's older Docker;
# 3. with an NVIDIA driver installed, NVIDIA's Container Toolkit and the CDI
#    spec (/etc/cdi/nvidia.yaml) that gives containers the GPU. It never
#    installs or changes a GPU driver;
# 4. the node's host files (deploy/node/host/install.sh): udev rules, the
#    Steam sandbox's AppArmor profile, the uinput and uhid modules;
# 5. the data root (CHA_DATA_ROOT, /srv/cha-portal), owned by root, mode
#    0755; the agent makes each user's directories in it itself;
# 6. deploy/quickstart/.env with CHA_VERSION, owned by you, mode 0600;
# 7. the environment images: the release's published ones, pulled, or with
#    --build built from this checkout;
# 8. with --tailscale, Tailscale from its own repository.
# Then it prints what to run next. It changes nothing outside these.
set -eu

usage() {
    cat <<'USAGE'
Usage: setup.sh [--version X.Y.Z] [options]

Prepares this machine for the Cha Portal quick start. Run it with sudo from the
repository checkout.

  --version X.Y.Z   the release to run, written to deploy/quickstart/.env
                    (CHA_VERSION); needed the first time
  --data-root DIR   where app data lives (default /srv/cha-portal)
  --tailscale       also install Tailscale
  --no-nvidia       skip the NVIDIA Container Toolkit even with a driver
  --build           build the environment images from this checkout instead
                    of pulling the release's published ones
  --check           only report what would change; needs no root
  --help            this text
USAGE
}

version= data_root= tailscale=0 nvidia=1 build=0 check_only=0
while [ $# -gt 0 ]; do
    case $1 in
        --version) version=${2:?--version needs a value}; shift ;;
        --version=*) version=${1#*=} ;;
        --data-root) data_root=${2:?--data-root needs a value}; shift ;;
        --data-root=*) data_root=${1#*=} ;;
        --tailscale) tailscale=1 ;;
        --no-nvidia) nvidia=0 ;;
        --build) build=1 ;;
        --check) check_only=1 ;;
        --help | -h) usage; exit 0 ;;
        *) echo "setup.sh: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
env_file=$here/.env
min_docker=26.0
# Where releases publish the images (.github/workflows/publish.yml).
registry=ghcr.io/ban-red

ok() { echo "ok         $*"; }
todo() { echo "todo       $*"; changed=1; }
did() { echo "done       $*"; }
skip() { echo "skipped    $*"; }
warn() { echo "warning    $*" >&2; }
die() { echo "setup.sh: $*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }
changed=0
failed=0   # a step that failed but didn't stop the rest

# Preflight: the machine and the checkout.
[ "$(uname -s)" = Linux ] || die "the quick start runs on Linux (this is $(uname -s))"
[ "$(uname -m)" = x86_64 ] || die "the images are built for x86-64 (this is $(uname -m))"
[ -r /etc/os-release ] || die "no /etc/os-release: can't tell the distribution"
. /etc/os-release
case "$ID ${ID_LIKE:-}" in
    *ubuntu* | *debian*) ;;
    *) die "this script knows Ubuntu and Debian (this is $ID); follow SETUP.md by hand" ;;
esac
distro=$ID
case ${ID_LIKE:-} in *ubuntu*) distro=ubuntu ;; esac
[ "$distro" = ubuntu ] || [ "$distro" = debian ] || distro=debian
codename=${UBUNTU_CODENAME:-${VERSION_CODENAME:-}}
[ -n "$codename" ] || die "can't tell the release codename from /etc/os-release"
[ -f "$repo/deploy/node/host/install.sh" ] || die "run this from the repository checkout"
if [ "$check_only" = 0 ] && [ "$(id -u)" != 0 ]; then
    die "this installs packages and changes /etc, so run it as root: sudo $0
(--check reports without root)"
fi
owner=${SUDO_USER:-root}

apt_updated=0
apt_install() {
    if [ "$apt_updated" = 0 ]; then
        DEBIAN_FRONTEND=noninteractive apt-get update -q
        apt_updated=1
    fi
    DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends "$@"
}
# add_repo NAME KEY_URL LINE: a signed apt repository, its key kept in
# /etc/apt/keyrings/NAME.gpg (LINE says signed-by=KEYRING). Keys come armored
# (Docker, NVIDIA) or not (Tailscale); apt wants them binary here.
add_repo() {
    keyring=/etc/apt/keyrings/$1.gpg
    key=$(mktemp)
    install -d -m 0755 /etc/apt/keyrings
    curl -fsSL "$2" -o "$key"
    if head -c 10 "$key" | grep -q -- '-----BEGIN'; then
        gpg --dearmor --yes -o "$keyring" "$key"
    else
        cp "$key" "$keyring"
    fi
    rm -f "$key"
    chmod 0644 "$keyring"
    echo "$3" | sed "s#KEYRING#$keyring#" >"/etc/apt/sources.list.d/$1.list"
    apt_updated=0
}

echo "==> packages"
missing=
for pkg in ca-certificates curl gnupg git kmod udev apparmor python3; do
    dpkg-query -W -f='${Status}' "$pkg" 2>/dev/null | grep -q 'ok installed' || missing="$missing $pkg"
done
if [ -z "$missing" ]; then
    ok "base packages"
elif [ "$check_only" = 1 ]; then
    todo "install$missing"
else
    # shellcheck disable=SC2086 # one word per package
    apt_install $missing
    did "installed$missing"
fi
if have timedatectl; then
    if [ "$(timedatectl show -p NTP --value 2>/dev/null)" = yes ]; then
        ok "clock synchronised (NTP)"
    elif [ "$check_only" = 1 ]; then
        todo "turn on clock synchronisation (timedatectl set-ntp true)"
    else
        timedatectl set-ntp true && did "turned on clock synchronisation" ||
            warn "couldn't turn on NTP: keep the clock right some other way (media tokens last 60 s)"
    fi
else
    skip "clock synchronisation (no timedatectl): keep the clock right yourself"
fi

echo "==> Docker"
docker_version=
if have docker; then
    docker_version=$(docker version --format '{{.Server.Version}}' 2>/dev/null || true)
fi
if [ -n "$docker_version" ] && dpkg --compare-versions "$docker_version" ge "$min_docker"; then
    ok "Docker Engine $docker_version"
elif dpkg-query -W -f='${Status}' docker.io 2>/dev/null | grep -q 'ok installed'; then
    die "this machine has the distribution's Docker (docker.io${docker_version:+, $docker_version}), and the
quick start needs Docker's own, $min_docker or newer. Remove it first (sudo apt-get remove docker.io
docker-compose; images and volumes stay), then run this again."
elif have docker && [ -z "$docker_version" ] && [ "$check_only" = 1 ]; then
    warn "Docker is installed, but its engine didn't answer: run as root to check its version"
elif have docker && [ -z "$docker_version" ]; then
    die "Docker is installed, but its engine didn't answer: start it (systemctl start docker)"
elif [ "$check_only" = 1 ]; then
    todo "install Docker Engine $min_docker or newer from download.docker.com"
else
    add_repo docker "https://download.docker.com/linux/$distro/gpg" \
        "deb [arch=$(dpkg --print-architecture) signed-by=KEYRING] https://download.docker.com/linux/$distro $codename stable"
    apt_install docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
    systemctl enable --now docker >/dev/null 2>&1 || true
    docker_version=$(docker version --format '{{.Server.Version}}' 2>/dev/null) ||
        die "Docker is installed, but its engine didn't start: see systemctl status docker"
    did "installed Docker Engine $docker_version"
fi
if [ -n "$docker_version" ]; then
    for plugin in compose buildx; do
        if docker "$plugin" version >/dev/null 2>&1; then
            ok "docker $plugin"
        elif [ "$check_only" = 1 ]; then
            todo "install docker-$plugin-plugin"
        else
            apt_install "docker-$plugin-plugin"
            did "installed docker-$plugin-plugin"
        fi
    done
fi

echo "==> NVIDIA"
if [ "$nvidia" = 0 ]; then
    skip "NVIDIA (--no-nvidia)"
elif ! have nvidia-smi || ! nvidia-smi -L >/dev/null 2>&1; then
    skip "NVIDIA: no working driver (nvidia-smi). Install the driver yourself for an NVIDIA GPU;
           without one, delete the agent's devices: lines in deploy/quickstart/compose.yaml"
else
    ok "driver: $(nvidia-smi --query-gpu=name,driver_version --format=csv,noheader | head -1)"
    if have nvidia-ctk; then
        ok "NVIDIA Container Toolkit $(nvidia-ctk --version 2>/dev/null | head -1 | sed 's/.*version //')"
    elif [ "$check_only" = 1 ]; then
        todo "install the NVIDIA Container Toolkit"
    else
        add_repo nvidia-container-toolkit https://nvidia.github.io/libnvidia-container/gpgkey \
            "deb [signed-by=KEYRING] https://nvidia.github.io/libnvidia-container/stable/deb/\$(ARCH) /"
        apt_install nvidia-container-toolkit
        did "installed the NVIDIA Container Toolkit"
    fi
    # The spec names the driver's files, so it is made again after every
    # driver update; regenerating an unchanged one is harmless.
    if [ "$check_only" = 1 ]; then
        if [ -f /etc/cdi/nvidia.yaml ]; then
            ok "CDI spec /etc/cdi/nvidia.yaml (setup.sh regenerates it)"
        else
            todo "generate the CDI spec /etc/cdi/nvidia.yaml"
        fi
    elif have nvidia-ctk; then
        nvidia-ctk cdi generate --output=/etc/cdi/nvidia.yaml >/dev/null 2>&1 ||
            die "nvidia-ctk cdi generate failed: run it by hand to see why"
        nvidia-ctk cdi list 2>/dev/null | grep -qx 'nvidia.com/gpu=all' ||
            die "the CDI spec has no nvidia.com/gpu=all device"
        did "generated /etc/cdi/nvidia.yaml"
    fi
    # Docker reads CDI specs by default from 28.2; older engines need it on.
    if [ -n "$docker_version" ] && dpkg --compare-versions "$docker_version" lt 28.2; then
        if [ -f /etc/docker/daemon.json ] && python3 -c 'import json,sys; sys.exit(0 if json.load(open("/etc/docker/daemon.json")).get("features",{}).get("cdi") else 1)' 2>/dev/null; then
            ok "CDI on in Docker"
        elif [ "$check_only" = 1 ]; then
            todo "turn CDI on in /etc/docker/daemon.json (Docker $docker_version)"
        else
            [ -f /etc/docker/daemon.json ] && cp /etc/docker/daemon.json "/etc/docker/daemon.json.bak.$(date +%s)"
            python3 - <<'PY'
import json, os
path = "/etc/docker/daemon.json"
config = json.load(open(path)) if os.path.exists(path) else {}
config.setdefault("features", {})["cdi"] = True
os.makedirs("/etc/docker", exist_ok=True)
with open(path, "w") as f:
    json.dump(config, f, indent=2)
    f.write("\n")
PY
            systemctl restart docker
            did "turned CDI on in /etc/docker/daemon.json (the old file is kept beside it)"
        fi
    fi
fi

echo "==> host files"
if [ "$check_only" = 1 ]; then
    "$repo/deploy/node/host/install.sh" --check || changed=1
elif ! "$repo/deploy/node/host/install.sh"; then
    warn "the host files didn't all install (above); the rest goes on"
    failed=1
else
    for module in uinput uhid; do
        [ -e "/sys/module/$module" ] && continue
        # Cloud and minimal kernels keep some modules in linux-modules-extra.
        extra=linux-modules-extra-$(uname -r)
        if [ "$distro" = ubuntu ] && apt-cache show "$extra" >/dev/null 2>&1; then
            apt_install "$extra" && modprobe "$module" && did "loaded $module (from $extra)"
        fi
        [ -e "/sys/module/$module" ] || [ -e "/dev/$module" ] ||
            warn "no $module module: virtual gamepads need it (CHA_$(echo "$module" | tr a-z A-Z)= in .env goes without)"
    done
fi

echo "==> data root"
if [ -z "$data_root" ] && [ -f "$env_file" ]; then
    data_root=$(sed -n 's/^CHA_DATA_ROOT=//p' "$env_file" | tail -1)
fi
data_root=${data_root:-/srv/cha-portal}
case $data_root in /?*) ;; *) die "the data root must be an absolute path: $data_root" ;; esac
for dir in "$data_root" "$data_root/users" "$data_root/shared"; do
    if [ -d "$dir" ]; then
        ok "$dir ($(stat -c '%U:%G %a' "$dir"))"
    elif [ "$check_only" = 1 ]; then
        todo "make $dir (root:root 0755)"
    else
        install -d -m 0755 -o root -g root "$dir"
        did "made $dir (root:root 0755)"
    fi
done
if [ -d "$data_root" ]; then
    free=$(df -Pk "$data_root" | awk 'NR == 2 { print int($4 / 1048576) }')
    if [ "$free" -lt 20 ]; then
        warn "only ${free} GB free under $data_root: Steam games need far more"
    else
        ok "${free} GB free under $data_root"
    fi
fi

echo "==> settings"
current=
[ -f "$env_file" ] && current=$(sed -n 's/^CHA_VERSION=//p' "$env_file" | tail -1)
version=${version:-$current}
[ -n "$version" ] || die "say which release to run: --version 0.1.0 (see the repository's releases)"
version=${version#v}
if [ "$current" = "$version" ]; then
    ok "$env_file: CHA_VERSION=$version"
elif [ "$check_only" = 1 ]; then
    todo "set CHA_VERSION=$version in $env_file"
else
    if [ ! -f "$env_file" ]; then
        install -m 0600 "$here/.env.example" "$env_file"
    fi
    if grep -q '^CHA_VERSION=' "$env_file"; then
        sed -i "s/^CHA_VERSION=.*/CHA_VERSION=$version/" "$env_file"
    else
        echo "CHA_VERSION=$version" >>"$env_file"
    fi
    did "set CHA_VERSION=$version in $env_file"
fi
if [ "$data_root" != /srv/cha-portal ] && [ "$check_only" = 0 ] && ! grep -q "^CHA_DATA_ROOT=$data_root\$" "$env_file"; then
    sed -i '/^CHA_DATA_ROOT=/d' "$env_file"
    echo "CHA_DATA_ROOT=$data_root" >>"$env_file"
    did "set CHA_DATA_ROOT=$data_root in $env_file"
fi
# CHA_IMAGE_REGISTRY makes the agent run the published environment images
# (CHA_VERSION's); without it, it runs the local builds.
published=
[ -f "$env_file" ] && published=$(sed -n 's/^CHA_IMAGE_REGISTRY=//p' "$env_file" | tail -1)
if [ "$build" = 0 ]; then
    if [ "$published" = "$registry" ]; then
        ok "$env_file: CHA_IMAGE_REGISTRY=$registry (published environment images)"
    elif [ "$check_only" = 1 ]; then
        todo "set CHA_IMAGE_REGISTRY=$registry in $env_file"
    else
        sed -i '/^CHA_IMAGE_REGISTRY=/d' "$env_file"
        echo "CHA_IMAGE_REGISTRY=$registry" >>"$env_file"
        did "set CHA_IMAGE_REGISTRY=$registry in $env_file: the agent runs the published environment images"
    fi
elif [ -n "$published" ]; then
    if [ "$check_only" = 1 ]; then
        todo "remove CHA_IMAGE_REGISTRY from $env_file (--build: the images built here)"
    else
        sed -i '/^CHA_IMAGE_REGISTRY=/d' "$env_file"
        did "removed CHA_IMAGE_REGISTRY from $env_file: the agent runs the images built here"
    fi
fi
if [ "$check_only" = 0 ]; then
    chmod 0600 "$env_file"
    chown "$owner" "$env_file"
fi
# Images built from this checkout talk to the streamer, so the checkout
# should be the same release. (The host files come from it too.)
tag=$(git -c safe.directory="$repo" -C "$repo" describe --tags --exact-match 2>/dev/null || true)
if [ "$tag" = "v$version" ]; then
    ok "checkout is v$version"
elif [ "$build" = 0 ]; then
    echo "info       the checkout is ${tag:-not a release tag}; the published images are v$version's"
else
    warn "the checkout is ${tag:-not a release tag}, not v$version: the environment images
           built from it may not match the streamer. Run: git checkout v$version"
fi

echo "==> environment images"
if [ "$build" = 0 ]; then
    if [ -z "$docker_version" ]; then
        skip "pulling (no Docker yet)"
    else
        # The catalog's cha/env-<name>:dev, as the agent maps it.
        images=$(python3 -c 'import json, sys
for t in json.load(open(sys.argv[1]))["templates"]:
    name = t["image"].removeprefix("cha/").split(":")[0]
    print(f"{sys.argv[2]}/cha-{name}:{sys.argv[3]}")' "$repo/images/catalog.json" "$registry" "$version")
        for image in $images; do
            if docker image inspect "$image" >/dev/null 2>&1; then
                ok "$image"
            elif [ "$check_only" = 1 ]; then
                todo "pull $image"
            elif docker pull -q "$image" >/dev/null; then
                did "pulled $image"
            else
                warn "couldn't pull $image: is v$version published, and the package public?"
                failed=1
            fi
        done
    fi
elif [ -z "$docker_version" ]; then
    skip "building (no Docker yet)"
elif [ "$check_only" = 1 ]; then
    echo "info       setup.sh builds them (docker compose -f images/compose.yaml build)"
else
    echo "Building Chrome, Firefox, XFCE, KDE, Steam and the test pattern: this takes a while."
    docker compose -f "$repo/images/compose.yaml" build
    did "built the environment images"
fi

if [ "$tailscale" = 1 ]; then
    echo "==> Tailscale"
    if have tailscale; then
        ok "Tailscale $(tailscale version 2>/dev/null | head -1)"
    elif [ "$check_only" = 1 ]; then
        todo "install Tailscale"
    else
        add_repo tailscale "https://pkgs.tailscale.com/stable/$distro/$codename.noarmor.gpg" \
            "deb [signed-by=KEYRING] https://pkgs.tailscale.com/stable/$distro $codename main"
        apt_install tailscale
        systemctl enable --now tailscaled >/dev/null 2>&1 || true
        did "installed Tailscale"
    fi
fi

echo
if [ "$check_only" = 1 ]; then
    if [ "$changed" = 1 ]; then
        echo "Out of date: run sudo $0"
        exit 1
    fi
    echo "Everything is in place."
    exit 0
fi
if [ "$failed" = 1 ]; then
    echo "Not finished: fix the warnings above, then run sudo $0 again." >&2
    exit 1
fi
compose="docker compose -f $repo/deploy/quickstart/compose.yaml"
cat <<NEXT
Done. Next (SETUP.md, "Quick start"):

  $compose up -d portal
  sudo tailscale up                 # once, if this machine isn't on your tailnet
  sudo tailscale serve --bg 7676    # HTTPS; 'tailscale serve status' prints the URL

Open the portal's URL and claim it: the first visitor creates the first
admin. Then add a node in the portal (Admin -> Nodes -> Add node) and start the agent:

  CHA_JOIN_TOKEN=chajoin_... $compose up -d
  $compose run --rm agent --doctor
NEXT
if [ "$owner" != root ] && ! id -nG "$owner" | grep -qw docker; then
    echo
    echo "Those docker commands need sudo as $owner. Adding $owner to the docker group"
    echo "avoids that, but makes $owner root-equivalent on this machine."
fi
