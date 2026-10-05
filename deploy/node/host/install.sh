#!/bin/sh
# Installs the host files a Cha Node needs, from this checkout. Run it with
# sudo on the node, from the repository (and again after an update):
#   sudo deploy/node/host/install.sh
#   deploy/node/host/install.sh --check     # only reports; needs no root
#
# What it installs, each only when it differs from what is there:
# - the udev rules that keep virtual gamepads off the host's seat
#   (/etc/udev/rules.d), then reloads udev and re-applies them to input and
#   hidraw devices;
# - the AppArmor profile(s) Steam environments run under (/etc/apparmor.d),
#   loaded with apparmor_parser; skipped on a host without AppArmor;
# - /etc/modules-load.d/cha.conf, so uinput and uhid load at boot; both are
#   loaded now too.
# This is the owner's step: the agent and its containers never write these.
#
# CHA_HOST_ROOT is a prefix for every destination path (the tests use it).
set -eu

usage() {
    cat <<'USAGE'
Usage: install.sh [--check]

Installs the udev rules, AppArmor profile and module list a Cha Node needs on
its host. Run it with sudo from the repository checkout.

  --check   only report what would change; exit 1 if anything would, 0 if
            everything is current. Needs no root.
  --help    this text
USAGE
}

check_only=0
for arg in "$@"; do
    case $arg in
        --check) check_only=1 ;;
        --help | -h) usage; exit 0 ;;
        *) echo "install.sh: unknown argument: $arg" >&2; usage >&2; exit 2 ;;
    esac
done

here=$(cd "$(dirname "$0")" && pwd)
root=${CHA_HOST_ROOT:-}

if [ "$check_only" = 0 ] && [ -z "$root" ] && [ "$(id -u)" != 0 ]; then
    echo "install.sh: this changes files under /etc, so run it as root:" >&2
    echo "  sudo deploy/node/host/install.sh" >&2
    echo "(--check reports without root)" >&2
    exit 1
fi

changed=0   # anything installed or updated (or, with --check, that would be)
failed=0

# sync SRC DEST: installs SRC at DEST (mode 644) when it differs. Sets
# $result to installed, updated or unchanged; prints the line.
sync_file() {
    src=$1 dest=$2
    if [ ! -e "$dest" ]; then
        result=installed
    elif cmp -s "$src" "$dest"; then
        result=unchanged
    else
        result=updated
    fi
    if [ "$result" = unchanged ]; then
        echo "unchanged  $dest"
        return
    fi
    changed=1
    if [ "$check_only" = 1 ]; then
        case $result in installed) echo "missing    $dest" ;; *) echo "outdated   $dest" ;; esac
        return
    fi
    mkdir -p "$(dirname "$dest")"
    install -m 644 "$src" "$dest"
    echo "$result  $dest"
}

# A tool that failed doesn't stop the rest; the exit status says so.
run() {
    if ! "$@"; then
        echo "install.sh: failed: $*" >&2
        failed=1
    fi
}

# udev rules
rules_changed=0
for src in "$here"/*.rules; do
    sync_file "$src" "$root/etc/udev/rules.d/$(basename "$src")"
    [ "$result" = unchanged ] || rules_changed=1
done
if [ "$rules_changed" = 1 ] && [ "$check_only" = 0 ]; then
    if command -v udevadm >/dev/null 2>&1; then
        run udevadm control --reload
        run udevadm trigger --subsystem-match=input --subsystem-match=hidraw
    else
        echo "skipped    udev reload (no udevadm; reboot to apply the rules)"
    fi
fi

# AppArmor profiles
if [ -d "$root/etc/apparmor.d" ] && command -v apparmor_parser >/dev/null 2>&1; then
    for src in "$here"/apparmor/*; do
        [ -f "$src" ] || continue
        dest=$root/etc/apparmor.d/$(basename "$src")
        sync_file "$src" "$dest"
        if [ "$result" != unchanged ] && [ "$check_only" = 0 ]; then
            run apparmor_parser -r -W "$dest"
        fi
    done
else
    echo "skipped    AppArmor profiles (this host has no AppArmor)"
fi

# Kernel modules
sync_file "$here/modules-load.d/cha.conf" "$root/etc/modules-load.d/cha.conf"
if [ "$check_only" = 0 ]; then
    if command -v modprobe >/dev/null 2>&1; then
        run modprobe uinput
        run modprobe uhid
    else
        echo "skipped    modprobe (not found; the modules load at next boot)"
    fi
fi

if [ "$failed" = 1 ]; then
    exit 1
fi
if [ "$check_only" = 1 ] && [ "$changed" = 1 ]; then
    echo "Out of date: run sudo deploy/node/host/install.sh"
    exit 1
fi
exit 0
