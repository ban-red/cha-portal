#!/usr/bin/env bash
# Spike S6: picture quality of PyroWave against NVENC on desktop content
# (see README.md). Runs inside the s6-quality image; results go to /out.
set -euo pipefail

W=2560 H=1440 FPS=60
# The streamer's defaults at 1440p60: 40 Mbit/s for the hardware codecs,
# 290 Mbit/s for PyroWave 4:2:0 and twice that for 4:4:4.
HW_BPS=40000000
P420_BYTES=$((290000000 / 8 / FPS))
P444_BYTES=$((P420_BYTES * 2))
# name:frames:pixels scrolled per frame
SCENARIOS="static:60:0 scroll:120:10 flick:120:40"
CODECS="h264 hevc pyrowave420 pyrowave444"

OUT=/out/s6-$(date -u +%Y%m%d-%H%M%S)
WORK=$OUT/work
# Scratch (several GB of raw video) never outlives a run, failed or not; the
# results belong to whoever owns the results directory.
rm -rf /out/*/work
find /out -mindepth 1 -maxdepth 1 -type d -empty -delete
mkdir -p "$WORK"
trap 'rm -rf "$WORK"; chown -R "$(stat -c %u:%g /out)" "$OUT" 2>/dev/null || true' EXIT
log() { printf '\n== %s\n' "$*"; }
ff() { ffmpeg -hide_banner -v error -y "$@"; }

log "rendering the test page with Chrome (${W} px wide)"
google-chrome --headless=new --no-sandbox --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
  --window-size=$W,7700 --screenshot="$WORK/page.png" file:///spike/page.html >/dev/null 2>&1
ffprobe -v error -show_entries stream=width,height -of csv=p=0 "$WORK/page.png"

encode() { # codec, scenario, frames → $WORK/<scenario>.<codec>.y4m, prints bytes per frame
  local codec=$1 sc=$2 frames=$3 src
  case $codec in
    h264|hevc)
      src=$WORK/$sc.420.y4m
      ff -i "$src" -c:v "${codec}_nvenc" -preset p1 -tune ull -rc cbr -b:v $HW_BPS -maxrate $HW_BPS \
        -bufsize $((HW_BPS / FPS)) -rc_init_occupancy $((HW_BPS / FPS)) -g 1000000 -bf 0 \
        -zerolatency 1 -delay 0 -colorspace bt709 -color_primaries bt709 -color_trc bt709 -color_range tv \
        -f "$codec" "$WORK/$sc.$codec.bit"
      echo $(($(stat -c %s "$WORK/$sc.$codec.bit") / frames))
      ff -f "$codec" -i "$WORK/$sc.$codec.bit" -pix_fmt yuv420p -f yuv4mpegpipe "$WORK/$sc.$codec.y4m"
      rm "$WORK/$sc.$codec.bit"
      ;;
    pyrowave420)
      pyrowave-quality "$WORK/$sc.420.y4m" "$WORK/$sc.$codec.y4m" $P420_BYTES | sed -E 's/.* ([0-9]+) bytes per frame.*/\1/'
      ;;
    pyrowave444)
      pyrowave-quality "$WORK/$sc.444.y4m" "$WORK/$sc.$codec.y4m" $P444_BYTES | sed -E 's/.* ([0-9]+) bytes per frame.*/\1/'
      ;;
  esac
}

# Mean PSNR per plane and SSIM against the 4:4:4 source; 4:2:0 output is
# upsampled first, so its lost chroma counts.
metrics() { # scenario, codec → "psnr_y psnr_u psnr_v psnr_y_min psnr_y_first ssim_y ssim_all"
  local sc=$1 codec=$2
  ff -i "$WORK/$sc.$codec.y4m" -i "$WORK/$sc.444.y4m" -lavfi \
    "[0:v]format=yuv444p,split[a][b];[1:v]split[c][d];[a][c]psnr=stats_file=$WORK/psnr.log;[b][d]ssim=stats_file=$WORK/ssim.log" \
    -f null -
  local psnr ssim
  psnr=$(awk '{ for (i = 1; i <= NF; i++) { split($i, kv, ":"); v = (kv[2] == "inf") ? 99 : kv[2]; f[kv[1]] = v }
                y += f["psnr_y"]; u += f["psnr_u"]; vv += f["psnr_v"]; n++
                if (n == 1 || f["psnr_y"] < min) min = f["psnr_y"]; if (n == 1) first = f["psnr_y"] }
              END { printf "%.2f %.2f %.2f %.2f %.2f", y / n, u / n, vv / n, min, first }' "$WORK/psnr.log")
  ssim=$(awk '{ for (i = 1; i <= NF; i++) { split($i, kv, ":"); f[kv[1]] = kv[2] }; y += f["Y"]; a += f["All"]; n++ }
              END { printf "%.4f %.4f", y / n, a / n }' "$WORK/ssim.log")
  echo "$psnr $ssim"
}

# A 400×225 region of one frame, enlarged 3× without smoothing, labelled.
crop() { # source y4m, frame, x, y, label, out.png
  ff -i "$1" -vf "select=eq(n\,$2),scale=in_color_matrix=bt709:in_range=tv:out_color_matrix=bt709:out_range=pc,format=rgb24,crop=400:225:$3:$4,scale=1200:675:flags=neighbor,drawtext=fontfile=/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf:text='$5':x=12:y=10:fontsize=34:fontcolor=white:box=1:boxcolor=black@0.7:boxborderw=6" \
    -frames:v 1 "$6"
}

echo '[' > "$OUT/results.json"
first=1
{
  echo "| Scenario | Codec | Mbit/s | PSNR Y | PSNR Cb | PSNR Cr | PSNR Y min | PSNR Y frame 0 | SSIM Y | SSIM all |"
  echo "|---|---|---|---|---|---|---|---|---|---|"
} > "$OUT/summary.md"

for s in $SCENARIOS; do
  IFS=: read -r sc frames speed <<< "$s"
  log "$sc: $frames frames, scrolling $((speed * FPS)) px/s"
  ff -loop 1 -framerate $FPS -i "$WORK/page.png" -frames:v "$frames" \
    -vf "crop=$W:$H:0:'n*$speed',scale=out_color_matrix=bt709:out_range=tv,format=yuv444p" \
    -f yuv4mpegpipe "$WORK/$sc.444.y4m"
  ff -i "$WORK/$sc.444.y4m" -vf format=yuv420p -f yuv4mpegpipe "$WORK/$sc.420.y4m"
  for codec in $CODECS; do
    bytes=$(encode "$codec" "$sc" "$frames")
    read -r py pu pv pmin pfirst sy sall <<< "$(metrics "$sc" "$codec")"
    mbps=$(awk -v b="$bytes" -v f=$FPS 'BEGIN { printf "%.1f", b * 8 * f / 1e6 }')
    printf '%-7s %-12s %7s Mbit/s  PSNR Y %s Cb %s Cr %s (Y min %s, frame 0 %s)  SSIM Y %s all %s\n' \
      "$sc" "$codec" "$mbps" "$py" "$pu" "$pv" "$pmin" "$pfirst" "$sy" "$sall"
    echo "| $sc | $codec | $mbps | $py | $pu | $pv | $pmin | $pfirst | $sy | $sall |" >> "$OUT/summary.md"
    [ $first = 1 ] || echo ',' >> "$OUT/results.json"
    first=0
    printf '  {"scenario":"%s","codec":"%s","mbps":%s,"psnr_y":%s,"psnr_cb":%s,"psnr_cr":%s,"psnr_y_min":%s,"psnr_y_frame0":%s,"ssim_y":%s,"ssim_all":%s}' \
      "$sc" "$codec" "$mbps" "$py" "$pu" "$pv" "$pmin" "$pfirst" "$sy" "$sall" >> "$OUT/results.json"
    if [ "$sc" = scroll ]; then
      # Mid-scroll: prose (left column) and coloured code (right column).
      crop "$WORK/$sc.$codec.y4m" 60 420 640 "$codec" "$WORK/prose.$codec.png"
      crop "$WORK/$sc.$codec.y4m" 60 1440 300 "$codec" "$WORK/code.$codec.png"
    fi
    rm "$WORK/$sc.$codec.y4m"
  done
  if [ "$sc" = scroll ]; then
    crop "$WORK/$sc.444.y4m" 60 420 640 source "$WORK/prose.source.png"
    crop "$WORK/$sc.444.y4m" 60 1440 300 source "$WORK/code.source.png"
  fi
  rm "$WORK/$sc.444.y4m" "$WORK/$sc.420.y4m"
done
printf '\n]\n' >> "$OUT/results.json"

log "crops (scroll, frame 60): source, then each codec"
for region in prose code; do
  ff $(for c in source $CODECS; do printf -- '-i %s ' "$WORK/$region.$c.png"; done) \
    -filter_complex "hstack=inputs=5" "$WORK/$region.row.png"
done
ff -i "$WORK/prose.row.png" -i "$WORK/code.row.png" -filter_complex vstack "$OUT/crops.png"
cp "$WORK/page.png" "$OUT/page.png"

log "summary"
cat "$OUT/summary.md"
echo "Results: $OUT"
