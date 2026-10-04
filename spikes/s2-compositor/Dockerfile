# Spike S2: the gst-wayland-display compositor (Wolf's, MIT) on the node GPU,
# with Google Chrome as its Wayland client, encoded with NVENC.
# Targets: `bench` (milestone 1) and `streamer` (milestone 2).
# Build from the repository root (see compose.yaml).
FROM ubuntu:26.04 AS base
ENV DEBIAN_FRONTEND=noninteractive
# GStreamer 1.28 with nvcodec (S1e), the compositor's build deps, and the
# libraries NVIDIA's EGL/Vulkan drivers link against (the driver itself comes
# from the CDI spec). No Mesa Vulkan driver: a CPU device must not win.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl gnupg git build-essential pkg-config clang libclang-dev hwdata \
      libwayland-dev libinput-dev libxkbcommon-dev libgbm-dev libegl-dev libudev-dev libdrm-dev \
      gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
      libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libgstreamer-plugins-bad1.0-dev \
      python3 python3-gi gir1.2-gstreamer-1.0 gir1.2-gst-plugins-base-1.0 \
      libvulkan1 vulkan-tools libglvnd0 libegl1 libgl1 libgles2 libxext6 libx11-6 \
      fonts-dejavu-core procps \
    && rm -rf /var/lib/apt/lists/*

# The cuda feature links gstcuda-1.0 through pkg-config; some distros ship the
# library without its .pc (gst-wayland-display's ci/README).
RUN if ! pkg-config --exists gstreamer-cuda-1.0; then \
      lib=$(ls /usr/lib/x86_64-linux-gnu/libgstcuda-1.0.so.0* | head -1) \
      && ln -sf "$lib" /usr/lib/x86_64-linux-gnu/libgstcuda-1.0.so \
      && printf 'Name: gstreamer-cuda-1.0\nDescription: shim\nVersion: 1.28.0\nLibs: -lgstcuda-1.0\nCflags: -I/usr/include/gstreamer-1.0\nRequires: gstreamer-1.0 gstreamer-base-1.0 gstreamer-video-1.0\n' \
         > /usr/lib/x86_64-linux-gnu/pkgconfig/gstreamer-cuda-1.0.pc; \
    fi

RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
ENV PATH=/root/.cargo/bin:$PATH

ARG GWD_REPO=https://github.com/games-on-whales/gst-wayland-display.git
ARG GWD_REV=016b4fc
# Our fixes on top of $GWD_REV (see patches/; to be sent upstream).
COPY spikes/s2-compositor/patches /opt/gwd-patches
RUN git clone "$GWD_REPO" /opt/gwd && git -C /opt/gwd checkout -q "$GWD_REV" \
 && git -C /opt/gwd apply /opt/gwd-patches/*.patch \
 && cargo build --manifest-path /opt/gwd/Cargo.toml --release -p gst-plugin-wayland-display \
      --features cuda,wayland-display-core/cuda \
 && mkdir -p /opt/gst-plugins && cp /opt/gwd/target/release/libgstwaylanddisplaysrc.so /opt/gst-plugins/ \
 && rm -rf /opt/gwd/target /root/.cargo/registry

# Google Chrome (the user's chosen first environment), from Google's apt repo.
RUN install -d -m 0755 /etc/apt/keyrings \
 && curl -fsSL https://dl.google.com/linux/linux_signing_key.pub | gpg --dearmor -o /etc/apt/keyrings/google.gpg \
 && echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/google.gpg] https://dl.google.com/linux/chrome/deb/ stable main" \
      > /etc/apt/sources.list.d/google-chrome.list \
 && apt-get update && apt-get install -y --no-install-recommends google-chrome-stable \
 && rm -rf /var/lib/apt/lists/*

ENV GST_PLUGIN_PATH=/opt/gst-plugins \
    NVIDIA_DRIVER_CAPABILITIES=all \
    XDG_RUNTIME_DIR=/tmp/xdg
COPY spikes/s2-compositor/bench /bench

# Milestone 1: the one-shot compositor bench.
FROM base AS bench
ENTRYPOINT ["python3", "/bench/bench.py"]

# Milestone 2: the live streamer (compositor + Chrome -> NVENC -> WebRTC).
FROM base AS streamer-build
WORKDIR /src
COPY . .
RUN cargo build --release -p s2-streamer

FROM base AS streamer
COPY --from=streamer-build /src/target/release/s2-streamer /usr/local/bin/s2-streamer
EXPOSE 4495/tcp 4496/udp
ENTRYPOINT ["s2-streamer"]
