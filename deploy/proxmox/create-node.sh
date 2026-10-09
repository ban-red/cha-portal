#!/bin/sh
# Makes a Cha Node, or the whole quick start, in an LXC container on a
# Proxmox VE host, sharing the host's Intel or AMD GPU with it
# (deploy/proxmox/README.md). Run it as root on the Proxmox host; it needs
# only this file:
#   curl -fsSLO https://raw.githubusercontent.com/ban-red/cha-portal/v0.2.0/deploy/proxmox/create-node.sh
#   sh create-node.sh --version 0.2.0                        # portal + node
#   sh create-node.sh --version 0.2.0 --portal-url https://portal.example
#
# What it does:
# 1. on the host: loads uinput and uhid (and lists them in
#    /etc/modules-load.d/cha.conf for boot), since a container can't;
# 2. creates a privileged Debian container with nesting (Docker inside), the
#    host's /dev/dri, /dev/uinput and /dev/uhid passed in, and the input and
#    hidraw devices the streamer makes allowed;
# 3. in it: clones the release and runs deploy/quickstart/setup.sh, which
#    installs Docker and pulls the images;
# 4. on the host: the udev rules that keep virtual pads off the host's seat,
#    and hold an AMD GPU's video engine clocks at their top step (without
#    --no-video-clocks), from that checkout;
# 5. in it: starts the quick start, or the node alone with --portal-url.
# NVIDIA GPUs aren't passed to containers by this script: give a VM the GPU
# instead (README).
set -eu

usage() {
    cat <<'USAGE'
Usage: create-node.sh --version X.Y.Z [options]

Creates an LXC container on this Proxmox host running Cha Portal's quick start
(the portal and a node), or with --portal-url a node for an existing portal.

  --version X.Y.Z      the release to run (required)
  --id N               the container's ID (default: the next free one)
  --hostname NAME      its hostname, and the node's name (default cha-node)
  --cores N            CPU cores (default 4)
  --memory MB          memory (default 8192)
  --disk GB            root disk, which holds the images (default 64)
  --storage NAME       storage for the root disk (default local-lvm)
  --template-storage NAME
                       storage for the Debian template (default local)
  --bridge NAME        network bridge (default vmbr0)
  --ip CIDR            a static address, e.g. 192.168.1.50/24 (default DHCP)
  --gateway IP         the gateway, with --ip
  --data-dir DIR       a host directory for app data (users' homes, Steam's
                       library), mounted at /srv/cha-portal; default: on the
                       root disk
  --gpu renderDN       the render node to pass in (default: every Intel and
                       AMD one); --no-gpu passes none, for a CPU-only node
  --portal-url URL     make a node for this portal, not the quick start
  --join-token TOKEN   with --portal-url: enroll with this token instead of
                       being claimed with a pairing code
  --insecure-portal    with --portal-url: allow plain http:// to another
                       machine (a trusted LAN only)
  --tailscale          pass /dev/net/tun in and install Tailscale, for HTTPS
  --enable-huc         on an Intel GPU of 6th to 10th generation Core, have the host
                       load its HuC firmware at boot (the low-power encoder's
                       bitrate control needs it); takes effect after a reboot
  --no-video-clocks    don't install the udev rule that holds an AMD GPU's video
                       engine clocks at their top step (it cuts the VA-API
                       encode time of a 1440p frame by half; it does nothing
                       on Intel)
  --plain-http         the quick start over plain HTTP on the LAN, with no
                       Secure cookie (a trusted LAN only: SETUP.md, Without
                       HTTPS)
  --no-pull            don't pull the environment images now; each downloads
                       on its first launch
  --ssh-key FILE       a public key for root's SSH login in the container
  --source DIR         copy this checkout on the host into the container
                       instead of cloning the release (offline, or to try a
                       change); --version still picks the published images
  --dry-run            print the container's settings and stop
  --help               this text
USAGE
}

version= id= hostname=cha-node cores=4 memory=8192 disk=64 storage=local-lvm
template_storage=local bridge=vmbr0 ip= gateway= data_dir= gpu=auto
portal_url= join_token= insecure=0 tailscale=0 plain_http=0 pull=1 ssh_key= source= dry_run=0 enable_huc=0 video_clocks=1
need() { [ $# -ge 2 ] && [ -n "$2" ] || { echo "create-node.sh: $1 needs a value" >&2; exit 2; }; }
while [ $# -gt 0 ]; do
    case $1 in
        --version) need "$@"; version=${2#v}; shift ;;
        --id) need "$@"; id=$2; shift ;;
        --hostname) need "$@"; hostname=$2; shift ;;
        --cores) need "$@"; cores=$2; shift ;;
        --memory) need "$@"; memory=$2; shift ;;
        --disk) need "$@"; disk=$2; shift ;;
        --storage) need "$@"; storage=$2; shift ;;
        --template-storage) need "$@"; template_storage=$2; shift ;;
        --bridge) need "$@"; bridge=$2; shift ;;
        --ip) need "$@"; ip=$2; shift ;;
        --gateway) need "$@"; gateway=$2; shift ;;
        --data-dir) need "$@"; data_dir=$2; shift ;;
        --gpu) need "$@"; gpu=${2#/dev/dri/}; shift ;;
        --no-gpu) gpu=none ;;
        --portal-url) need "$@"; portal_url=$2; shift ;;
        --join-token) need "$@"; join_token=$2; shift ;;
        --insecure-portal) insecure=1 ;;
        --tailscale) tailscale=1 ;;
        --enable-huc) enable_huc=1 ;;
        --no-video-clocks) video_clocks=0 ;;
        --plain-http) plain_http=1 ;;
        --no-pull) pull=0 ;;
        --ssh-key) need "$@"; ssh_key=$2; shift ;;
        --source) need "$@"; source=$2; shift ;;
        --dry-run) dry_run=1 ;;
        --help | -h) usage; exit 0 ;;
        *) echo "create-node.sh: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

say() { echo "==> $*"; }
warn() { echo "warning: $*" >&2; }
die() { echo "create-node.sh: $*" >&2; exit 1; }

repo_url=https://github.com/ban-red/cha-portal.git
checkout=/opt/cha-portal

# Preflight.
[ -n "$version" ] || die "say which release to run: --version 0.2.0 (see the repository's releases)"
command -v pct >/dev/null 2>&1 && command -v pveversion >/dev/null 2>&1 ||
    die "run this on a Proxmox VE host (no pct here)"
[ "$(id -u)" = 0 ] || die "run it as root on the Proxmox host"
if [ -n "$join_token" ] || [ "$insecure" = 1 ]; then
    [ -n "$portal_url" ] || die "--join-token and --insecure-portal go with --portal-url"
fi
case $portal_url in
    '' | https://*) ;;
    http://*) [ "$insecure" = 1 ] || die "$portal_url is plain http: add --insecure-portal (a trusted LAN only), or use https://" ;;
    *) die "--portal-url must start with https:// (or http:// with --insecure-portal)" ;;
esac
[ "$plain_http" = 0 ] || [ -z "$portal_url" ] || die "--plain-http is for the quick start; the portal's own settings decide that"
[ -z "$gateway" ] || [ -n "$ip" ] || die "--gateway goes with --ip"
[ -z "$data_dir" ] || [ -d "$data_dir" ] || die "--data-dir $data_dir isn't a directory on this host"
[ -z "$ssh_key" ] || [ -r "$ssh_key" ] || die "can't read $ssh_key"
[ -z "$source" ] || [ -f "$source/deploy/quickstart/setup.sh" ] || die "--source $source isn't a Cha Portal checkout"
[ -n "$id" ] || id=$(pvesh get /cluster/nextid)
if pct status "$id" >/dev/null 2>&1; then
    die "container $id already exists: pick another with --id"
fi

# The render nodes to pass in: Intel (0x8086) and AMD (0x1002) ones.
gpus=
nvidia=0
for dev in /sys/class/drm/renderD*; do
    [ -e "$dev" ] || continue
    name=${dev##*/}
    vendor=$(cat "$dev/device/vendor" 2>/dev/null || echo unknown)
    case $vendor in
        0x10de) nvidia=1; continue ;;
        0x8086 | 0x1002) ;;
        *) continue ;;
    esac
    if [ "$gpu" = auto ] || [ "$gpu" = "$name" ]; then
        gpus="$gpus $name"
    fi
done
gpus=${gpus# }
if [ "$gpu" != auto ] && [ "$gpu" != none ] && [ -z "$gpus" ]; then
    die "$gpu isn't an Intel or AMD render node on this host (ls /sys/class/drm)"
fi
[ "$gpu" = none ] && gpus=
if [ -z "$gpus" ] && [ "$gpu" != none ]; then
    warn "no Intel or AMD GPU on this host: the node will be CPU-only (H.264, modest sizes)"
fi
if [ "$nvidia" = 1 ]; then
    warn "this host has an NVIDIA GPU, which this script doesn't pass to a container: for it,
         give a VM the GPU and run the quick start there (deploy/proxmox/README.md)"
fi

# The hidraw major is dynamic; the streamer makes its DualSense and Steam
# Controller nodes with it.
hidraw_major=$(awk '$2 == "hidraw" { print $1 }' /proc/devices)

net="name=eth0,bridge=$bridge,ip=${ip:-dhcp}${gateway:+,gw=$gateway}"
say "container $id ($hostname): $cores cores, $memory MB, ${disk} GB on $storage, $net"
say "GPU: ${gpus:-none}${data_dir:+; app data: $data_dir}"
say "runs: ${portal_url:+a node for $portal_url}${portal_url:-the quick start (portal and node)}, release $version"
# Intel's HuC: on the GPUs of 6th to 10th generation Core (Skylake to Ice Lake) the
# kernel doesn't load it by default, and without it the low-power encoder
# offers no bitrate control, so the streamer uses the one that runs on the
# shader cores the apps draw with. i915's
# enable_guc=2 loads the HuC alone; 11th generation and newer load it anyway.
huc_info=$(cat /sys/kernel/debug/dri/*/gt0/uc/huc_info 2>/dev/null | head -1 || true)
huc_conf=/etc/modprobe.d/cha-i915-huc.conf
if [ "$huc_info" = "HuC disabled" ] && [ -n "$gpus" ]; then
    if [ "$enable_huc" = 1 ] && [ "$dry_run" = 1 ]; then
        say "HuC: would write $huc_conf (enable_guc=2) and update the initramfs; reboot the host after"
    elif [ "$enable_huc" = 1 ]; then
        say "the Intel GPU's HuC, from the next boot"
        if [ -f "$huc_conf" ]; then
            echo "have $huc_conf"
        else
            echo "options i915 enable_guc=2" >"$huc_conf"
            update-initramfs -u -k all >/dev/null
            echo "wrote $huc_conf and updated the initramfs: reboot the host to load the HuC"
        fi
    else
        warn "this Intel GPU's HuC isn't loaded, so streams encode on its shader cores. --enable-huc
         sets the host to load it (a reboot of the host is needed): deploy/proxmox/README.md"
    fi
fi

[ "$dry_run" = 1 ] && exit 0

# 1. The modules, on the host.
say "uinput and uhid on the host"
for module in uinput uhid; do
    [ -e "/dev/$module" ] || modprobe "$module" || warn "couldn't load $module: virtual gamepads need it"
done
if [ ! -f /etc/modules-load.d/cha.conf ]; then
    printf '# Cha Node virtual gamepads (deploy/proxmox/create-node.sh).\nuinput\nuhid\n' >/etc/modules-load.d/cha.conf
    echo "wrote /etc/modules-load.d/cha.conf"
fi

# 2. The container.
say "the Debian template"
pveam update >/dev/null 2>&1 || warn "pveam update failed; using the template list as it is"
template=$(pveam available --section system 2>/dev/null | awk '$2 ~ /^debian-13-standard_.*_amd64\./ { print $2 }' | sort -V | tail -1)
[ -n "$template" ] || die "no debian-13-standard template in pveam available"
if pveam list "$template_storage" 2>/dev/null | grep -q "$template"; then
    echo "have $template"
else
    pveam download "$template_storage" "$template"
fi

say "creating container $id"
set -- --hostname "$hostname" --cores "$cores" --memory "$memory" --swap 1024 \
    --rootfs "$storage:$disk" --net0 "$net" --unprivileged 0 \
    --features nesting=1,keyctl=1 --onboot 1 --ostype debian \
    --description "Cha Portal ${portal_url:+node}${portal_url:-quick start} $version (deploy/proxmox/create-node.sh)"
[ -n "$ssh_key" ] && set -- "$@" --ssh-public-keys "$ssh_key"
[ -n "$data_dir" ] && set -- "$@" --mp0 "$data_dir,mp=/srv/cha-portal"
pct create "$id" "$template_storage:vztmpl/$template" "$@"

conf=/etc/pve/lxc/$id.conf
{
    echo "# Cha Node devices (deploy/proxmox/create-node.sh)"
    if [ -n "$gpus" ]; then
        echo "lxc.cgroup2.devices.allow: c 226:* rwm"
        for name in $gpus; do
            echo "lxc.mount.entry: /dev/dri/$name dev/dri/$name none bind,optional,create=file"
            # Its primary node too: Steam's gamescope needs it.
            for card in /sys/class/drm/$name/device/drm/card*; do
                [ -e "$card" ] || continue
                card=${card##*/}
                echo "lxc.mount.entry: /dev/dri/$card dev/dri/$card none bind,optional,create=file"
            done
        done
    fi
    echo "lxc.cgroup2.devices.allow: c 10:223 rwm"
    echo "lxc.mount.entry: /dev/uinput dev/uinput none bind,optional,create=file"
    echo "lxc.cgroup2.devices.allow: c 10:239 rwm"
    echo "lxc.mount.entry: /dev/uhid dev/uhid none bind,optional,create=file"
    echo "lxc.cgroup2.devices.allow: c 13:* rwm"
    [ -n "$hidraw_major" ] && echo "lxc.cgroup2.devices.allow: c $hidraw_major:* rwm"
    if [ "$tailscale" = 1 ]; then
        echo "lxc.cgroup2.devices.allow: c 10:200 rwm"
        echo "lxc.mount.entry: /dev/net/tun dev/net/tun none bind,create=file"
    fi
} >>"$conf"
echo "added the devices to $conf"

say "starting it"
pct start "$id"
in_ct() { pct exec "$id" -- sh -c "$1"; }
tries=0
until in_ct 'getent hosts github.com >/dev/null 2>&1'; do
    tries=$((tries + 1))
    [ "$tries" -lt 60 ] || die "container $id has no network after a minute: check $bridge and DHCP"
    sleep 1
done

# 3. The release and setup.sh.
in_ct "export DEBIAN_FRONTEND=noninteractive; apt-get update -q >/dev/null && apt-get install -y -q --no-install-recommends ca-certificates git >/dev/null"
if [ -n "$source" ]; then
    say "$source, copied to $checkout"
    tar -C "$source" --exclude=./target --exclude=./node_modules --exclude=./data -cf - . |
        pct exec "$id" -- sh -c "mkdir -p $checkout && tar -C $checkout -xf -"
else
    say "the release, in $checkout"
    in_ct "[ -d $checkout/.git ] || git clone -q --branch v$version --depth 1 $repo_url $checkout"
fi

say "setup.sh in the container"
opts="--version $version --no-nvidia"
[ "$pull" = 0 ] && opts="$opts --no-pull"
[ "$tailscale" = 1 ] && opts="$opts --tailscale"
in_ct "$checkout/deploy/quickstart/setup.sh $opts" ||
    die "setup.sh didn't finish in container $id: fix what it says (pct enter $id), then run it again there"

# 4. The udev rules, on the host: virtual pads appear on the host's kernel.
say "udev rules on the host"
rules_list=72-cha-virtual-pads.rules
[ "$video_clocks" = 0 ] || rules_list="$rules_list 73-cha-amd-video-clocks.rules"
for rules in $rules_list; do
    pct pull "$id" "$checkout/deploy/node/host/$rules" "/etc/udev/rules.d/$rules"
    chmod 0644 "/etc/udev/rules.d/$rules"
    echo "installed /etc/udev/rules.d/$rules"
done
[ "$video_clocks" = 1 ] || echo "skipped /etc/udev/rules.d/73-cha-amd-video-clocks.rules (--no-video-clocks; a copy there is left alone)"
udevadm control --reload && udevadm trigger --subsystem-match=input --subsystem-match=hidraw &&
    { [ "$video_clocks" = 0 ] || udevadm trigger --action=add --subsystem-match=drm; } ||
    warn "couldn't reload udev; the rules apply after a reboot"
# The same files in the container, where the agent looks for them (they do
# nothing there; they say what the host has).
in_ct "install -d /etc/udev/rules.d /etc/modules-load.d && cd $checkout/deploy/node/host && cp $rules_list /etc/udev/rules.d/ && cp modules-load.d/cha.conf /etc/modules-load.d/"

# 5. Start it. No NVIDIA here, so the agent gets no CDI devices: a
# compose.override.yaml, which compose reads beside compose.yaml.
override='# No NVIDIA GPU in this container (deploy/proxmox/create-node.sh).
services:
  agent:
    devices: !reset []'
if [ -z "$portal_url" ]; then
    dir=$checkout/deploy/quickstart
    in_ct "printf '%s\n' '$override' >$dir/compose.override.yaml && sed -i '/^CHA_NODE_NAME=/d' $dir/.env && echo CHA_NODE_NAME=$hostname >>$dir/.env"
    if [ "$plain_http" = 1 ]; then
        in_ct "sed -i '/^CHA_SECURE_COOKIES=/d' $dir/.env && echo CHA_SECURE_COOKIES=false >>$dir/.env"
    fi
    say "starting the quick start"
    in_ct "cd $dir && docker compose up -d"
else
    dir=$checkout/deploy/node
    registry=ghcr.io/ban-red
    env="CHA_PORTAL_URL=$portal_url
CHA_NODE_NAME=$hostname
CHA_NODE_IMAGE=$registry/cha-node:$version
CHA_STREAMER_IMAGE=$registry/cha-streamer:$version
CHA_IMAGE_REGISTRY=$registry
CHA_IMAGE_TAG=$version
CHA_NVIDIA_WINE_DIR="
    [ "$insecure" = 1 ] && env="$env
CHA_ALLOW_INSECURE_PORTAL=true"
    [ -n "$join_token" ] && env="$env
CHA_JOIN_TOKEN=$join_token"
    in_ct "umask 077 && printf '%s\n' '$env' >$dir/.env && printf '%s\n' '$override' >$dir/compose.override.yaml"
    say "starting the node"
    in_ct "cd $dir && docker compose pull -q agent && docker compose up -d --no-build agent"
fi

address=$(in_ct "hostname -I" | awk '{ print $1 }')
echo
echo "Container $id ($hostname) is up at ${address:-its address}. Enter it with: pct enter $id"
echo "Its compose commands run from $dir (docker compose logs agent, ... run --rm agent --doctor)."
if [ -z "$portal_url" ]; then
    cat <<NEXT

Next:
  1. Give the portal HTTPS (gamepads, keyboard lock and sign-in need it):
     with --tailscale, run 'tailscale up' and 'tailscale serve --bg 7676' in
     the container (SETUP.md, Quick start). With --plain-http it is at
     http://$address:7676 instead. Open it straight away and claim it: the
     first visitor creates the first admin.
  2. Claim the node under Admin -> Nodes -> Found on your network, with the
     code from: pct exec $id -- sh -c 'cd $dir && docker compose logs agent | grep unclaimed'
NEXT
elif [ -z "$join_token" ]; then
    cat <<NEXT

Next: claim the node in the portal under Admin -> Nodes -> Found on your
network, with the code from:
  pct exec $id -- sh -c 'cd $dir && docker compose logs agent | grep unclaimed'
NEXT
fi
