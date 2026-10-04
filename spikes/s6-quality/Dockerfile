# Spike S6: picture quality of PyroWave against NVENC (H.264, HEVC) on desktop
# content (see README.md). It takes libpyrowave from the streamer image, so
# build that first:
#   docker build -f deploy/streamer/Dockerfile --target runtime -t cha/streamer:dev .
FROM cha/streamer:dev AS streamer

FROM ubuntu:26.04
ENV DEBIAN_FRONTEND=noninteractive
# ffmpeg for the NVENC encodes (h264_nvenc/hevc_nvenc load the driver's
# libnvidia-encode from CDI), decoding and the metrics; Chrome renders the
# test page. NVIDIA's Vulkan driver links libX11 and libXext.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl gnupg ffmpeg gcc libc6-dev libvulkan-dev libvulkan1 \
      libglvnd0 libegl1 libx11-6 libxext6 fonts-liberation \
 && install -d /etc/apt/keyrings \
 && curl -fsSL https://dl.google.com/linux/linux_signing_key.pub | gpg --dearmor -o /etc/apt/keyrings/google.gpg \
 && echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/google.gpg] https://dl.google.com/linux/chrome/deb/ stable main" \
      > /etc/apt/sources.list.d/google-chrome.list \
 && apt-get update && apt-get install -y --no-install-recommends google-chrome-stable \
 && rm -rf /var/lib/apt/lists/*

# The PyroWave round trip, against the library the streamer ships (same pin).
COPY --from=streamer /usr/local/lib/libpyrowave-shared.so.0 /usr/local/lib/
ARG PYROWAVE_REV=89f7e47d4abbf650c91fae766728af866c5e32a0
ADD https://raw.githubusercontent.com/Themaister/pyrowave/${PYROWAVE_REV}/pyrowave.h /opt/pyrowave/pyrowave.h
COPY spikes/s6-quality/pyrowave-quality.c /opt/pyrowave/
RUN gcc -O2 -Wall -I/opt/pyrowave -o /usr/local/bin/pyrowave-quality /opt/pyrowave/pyrowave-quality.c \
      /usr/local/lib/libpyrowave-shared.so.0 \
 && ldconfig

COPY spikes/s6-quality/page.html spikes/s6-quality/run.sh /spike/
ENV NVIDIA_DRIVER_CAPABILITIES=all
ENTRYPOINT ["bash", "/spike/run.sh"]
