#!/bin/sh
# Repairs a NAS share on a Proxmox host that went stale ("Stale file handle",
# or the kernel's "NFS: server ... error: fileid changed"), and the Cha Node
# containers that bind it (deploy/proxmox/README.md). Run it as root on the
# Proxmox host:
#   sh repair-mounts.sh --check          # only say what is wrong
#   sh repair-mounts.sh                  # repair /mnt/games
#   sh repair-mounts.sh --mount /mnt/nas --id 103
#
# What it does:
# 1. looks at the share on the host and in each container that binds it
#    (every look gives up after 10 s, since a hard NFS mount can hang);
# 2. if one is stale: lazily unmounts the share on the host and reads it so
#    its automount mounts it again;
# 3. restarts the containers that bind it, since a bind keeps the old mount
#    (this stops their environments, running Steam sessions included; the
#    node agent starts again with the container). --no-restart leaves them.
set -eu

usage() {
    cat <<'USAGE'
Usage: repair-mounts.sh [options]

  --mount DIR      the share's mount point on this host (default /mnt/games)
  --id N           only this container (default: each one that binds DIR)
  --check          report the state, change nothing; exits 1 if stale
  --no-restart     remount on the host, leave the containers as they are
USAGE
}

mount_dir=/mnt/games
only_id=
check=0
restart=1
while [ $# -gt 0 ]; do
    case "$1" in
        --mount) mount_dir=${2:?--mount needs a directory}; shift 2 ;;
        --id) only_id=${2:?--id needs a number}; shift 2 ;;
        --check) check=1; shift ;;
        --no-restart) restart=0; shift ;;
        -h | --help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done
[ "$(id -u)" -eq 0 ] || { echo "run as root on the Proxmox host" >&2; exit 2; }
command -v pct >/dev/null || { echo "pct not found: run this on a Proxmox VE host" >&2; exit 2; }

# Containers whose config binds the share: an `mpN: DIR,mp=...` line.
if [ -n "$only_id" ]; then
    ids=$only_id
else
    ids=$(grep -l -E "^mp[0-9]+: ${mount_dir}," /etc/pve/lxc/*.conf 2>/dev/null |
        sed 's|.*/\([0-9]*\)\.conf|\1|' | sort -n || true)
fi

host_ok() { timeout 10 ls "$mount_dir" >/dev/null 2>&1; }
ct_ok() { pct status "$1" 2>/dev/null | grep -q running && timeout 15 pct exec "$1" -- ls "$mount_dir" >/dev/null 2>&1; }
ct_running() { pct status "$1" 2>/dev/null | grep -q running; }

stale=0
if host_ok; then echo "host: $mount_dir reads fine"; else echo "host: $mount_dir is stale or unreachable"; stale=1; fi
for id in $ids; do
    ct_running "$id" || { echo "container $id: not running"; continue; }
    if ct_ok "$id"; then echo "container $id: $mount_dir reads fine"; else echo "container $id: $mount_dir is stale"; stale=1; fi
done

if [ "$stale" -eq 0 ]; then echo "nothing to repair"; exit 0; fi
[ "$check" -eq 0 ] || exit 1

echo "remounting $mount_dir on the host"
umount -l "$mount_dir" 2>/dev/null || true
i=0
until host_ok; do
    i=$((i + 1))
    [ "$i" -lt 5 ] || { echo "$mount_dir still unreadable on the host: is the NAS up and exporting it?" >&2; exit 1; }
    sleep 2
done
echo "host: $mount_dir reads fine again"

for id in $ids; do
    ct_running "$id" || continue
    ct_ok "$id" && continue
    if [ "$restart" -eq 0 ]; then
        echo "container $id still has the old mount: pct reboot $id"
        continue
    fi
    echo "restarting container $id (its environments stop)"
    pct reboot "$id"
    i=0
    until ct_ok "$id"; do
        i=$((i + 1))
        [ "$i" -lt 30 ] || { echo "container $id: $mount_dir not readable after the restart" >&2; exit 1; }
        sleep 2
    done
    echo "container $id: $mount_dir reads fine again"
done
echo "done: launch the environments again"
