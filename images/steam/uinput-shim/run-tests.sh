#!/bin/sh
# Builds the shim and its test programs for i386 and amd64 and runs them against
# the streamer's real broker (the `shim_*` tests in
# crates/cha-streamer/src/uinput_broker.rs): each program makes a pad through
# the fake /dev/uinput with the new and the legacy API, and one answers force
# feedback; the Rust side checks the kernel's device (28de:11ff) and that it
# goes away with the program.
#
# Needs a Linux box with gcc-multilib, cargo (the streamer's deps) and a
# /dev/uinput this user can write; with the device cgroup open to evdev
# (docker run --device /dev/uinput --device-cgroup-rule 'c 13:* rwm') the force
# feedback test can open the pad's node too. From the repository root:
#   images/steam/uinput-shim/run-tests.sh
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
out=${CHA_SHIM_OUT:-/tmp/cha-uinput-shim}
make -C "$here" OUT="$out" tests
cd "$root"
CHA_SHIM_TESTS="$out" cargo test -p cha-streamer uinput -- --nocapture --test-threads=1
