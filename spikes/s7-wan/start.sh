#!/usr/bin/env bash
# Inside the bench container: an unshaped link to begin with, then the
# streamer with the test pattern (and its sound), as the dev loop runs it.
# WebRTC binds the container's own address; the node's, which Docker
# forwards the ports from, is announced the way a router's would be.
set -euo pipefail
tc qdisc replace dev eth0 root handle 1: netem delay 0ms
export XDG_RUNTIME_DIR=/run/cha
own=$(hostname -i | awk '{print $1}')
exec /target/release/cha-streamer --token "" --token-file /tmp/token \
  --listen 0.0.0.0 --http-port 4515 --webrtc-port 4516 --wt-port 4517 \
  --advertise "$own" --public-address "$ADVERTISE" \
  --run /target/release/cha-testpattern
