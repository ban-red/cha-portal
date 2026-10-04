# Spike S1e: per-frame encode latency on the node GPU.
# NVENC (H.264/HEVC/AV1) through GStreamer's nvcodec elements, timed with the
# latency tracer; PyroWave through the WebGPU port's encoder on Vulkan.
# Build from the repository root (see compose.yaml).
#
# Ubuntu 26.04: 25.04's gst-plugins-bad ships no nvcodec plugin on amd64
# (LP #2109413, never fixed there) and 25.04 is end of life.
FROM ubuntu:26.04
ENV DEBIAN_FRONTEND=noninteractive
# libvulkan1 is only the loader. NVIDIA's Vulkan driver (libGLX_nvidia.so.0)
# and its ICD manifest come from the container toolkit's CDI spec (see
# compose.yaml); it needs libglvnd, libEGL and libXext from the image, or the
# loader skips it. No Mesa drivers: a CPU Vulkan device must not win.
RUN apt-get update && apt-get install -y --no-install-recommends \
      gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
      ffmpeg python3 perl ca-certificates git cmake ninja-build g++ \
      libvulkan1 vulkan-tools libglvnd0 libegl1 libgl1 libxext6 libx11-6 \
    && rm -rf /var/lib/apt/lists/* \
    && test -f /usr/lib/x86_64-linux-gnu/gstreamer-1.0/libgstnvcodec.so

# PyroWave encoder from the WebGPU port, against prebuilt wgpu-native (Vulkan backend on Linux).
ARG PYROWAVE_FORK=https://github.com/imbcmdth/pyrowave.git
ARG PYROWAVE_REV=5e80f92
RUN git clone --branch webgpu "$PYROWAVE_FORK" /src/pyrowave \
 && git -C /src/pyrowave checkout -q "$PYROWAVE_REV" \
 && perl -0pi -e 's/(\t\tCOMMENT "Embedding WGSL shaders")\)/$1\n\t\tVERBATIM)/' /src/pyrowave/webgpu/CMakeLists.txt \
 && cmake -P /src/pyrowave/webgpu/fetch_wgpu_native.cmake \
 && cmake -S /src/pyrowave/webgpu -B /src/build -G Ninja -DCMAKE_BUILD_TYPE=Release \
 && ninja -C /src/build \
 && cp /src/build/pyrowave-webgpu-encode /usr/local/bin/ \
 && rm -rf /src/build

COPY spikes/s1e-encode-latency/bench /bench
# NVENC needs the "video" driver capability, Vulkan needs "graphics".
ENV NVIDIA_DRIVER_CAPABILITIES=all
# A fresh plugin registry on every run, so nvcodec probes the GPU the
# container actually has.
ENV GST_REGISTRY=/tmp/gst-registry.bin
ENTRYPOINT ["python3", "/bench/bench.py"]
