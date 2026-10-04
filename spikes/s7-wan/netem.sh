#!/usr/bin/env bash
# Shapes the bench container's link (server → browser) through a scenario,
# one step per line: "<second> <rate> <netem arguments>", applied at that
# second after the start: the rate ("-": none), with a buffer of 150 ms at
# that rate, then netem's delay, jitter and loss. "<second> end" restores an unshaped link. Prints each
# step with the time it applied.
#   spikes/s7-wan/netem.sh spikes/s7-wan/scenarios/drop.txt
#
# Why the token bucket in front: QUIC hands the kernel up to 64 KB at a
# time (UDP GSO), segmented only after the qdisc. netem alone would count
# those as one packet each (a buffer of megabytes, and "1 % loss" dropping
# bursts of ten); the bucket splits them first.
#
# Jitter keeps packets in order, as a real queue does: netem gets the rate
# too, which makes each packet leave after the one before. S7_REORDER=1
# lets every packet take its own delay instead, which at a packet a
# millisecond and 5 ms of jitter reorders nearly all of them (a stress test;
# Chrome's WebRTC receiver stalls under it).
set -euo pipefail
scenario=$1
container=${S7_CONTAINER:-cha-s7-wan-1}
tc_() { docker exec "$container" tc "$@"; }
# Bits per second for a tc rate like 10mbit (unlimited: 10 Gbit/s).
bps() {
  local r=${1,,}
  case $r in
    -) echo 10000000000 ;;
    *gbit) echo $(( ${r%gbit} * 1000000000 )) ;;
    *mbit) echo $(( ${r%mbit} * 1000000 )) ;;
    *kbit) echo $(( ${r%kbit} * 1000 )) ;;
    *) echo "$r" ;;
  esac
}
shape() {
  local rate=$1
  shift
  local bits
  bits=$(bps "$rate")
  # 150 ms of ~1300-byte packets at the rate: the bottleneck's buffer.
  local limit=$(( bits * 15 / 100 / 8 / 1300 ))
  (( limit < 64 )) && limit=64
  (( limit > 10000 )) && limit=10000
  # A token bucket at the root (the rate; a small burst, so it splits QUIC's
  # GSO super-packets into real ones), netem under it (delay, jitter, loss,
  # and the buffer). Changed in place when already there, so queued packets
  # survive a step.
  local op=add
  if tc_ qdisc show dev eth0 | grep -q "qdisc tbf 1: root"; then
    op=change
  else
    tc_ qdisc del dev eth0 root 2>/dev/null || true
  fi
  tc_ qdisc $op dev eth0 root handle 1: tbf rate "$bits"bit burst 3kb latency 150ms
  local order=(rate "$bits"bit)
  [[ ${S7_REORDER:-0} == 1 ]] && order=()
  tc_ qdisc $op dev eth0 parent 1:1 handle 10: netem limit "$limit" "$@" "${order[@]}"
}
start=$(date +%s.%N)
while read -r at rate args; do
  [[ -z "$at" || "$at" == \#* ]] && continue
  now=$(date +%s.%N)
  wait=$(echo "$start + $at - $now" | bc)
  if (( $(echo "$wait > 0" | bc) )); then sleep "$wait"; fi
  if [[ "$rate" == end ]]; then
    rate=-
    args="delay 0ms"
  fi
  # shellcheck disable=SC2086
  shape "$rate" $args
  printf '%6.1f s  %s %s\n' "$(echo "$(date +%s.%N) - $start" | bc)" "$rate" "$args"
done < "$scenario"
