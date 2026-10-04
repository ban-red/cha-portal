# cha-streamer. Build context: the repository root.
#
# Targets:
#   dev      Rust, build deps and Google Chrome; the repository is bind-mounted
#            (compose.dev.yaml) to build and run the streamer in place.
#   runtime  the streamer alone, one container per environment:
#              docker build -f deploy/streamer/Dockerfile --target runtime -t cha/streamer:dev .
#
# The NVIDIA driver (EGL, GBM backend, CUDA, NVENC) comes from the CDI spec at
# run time; nothing NVIDIA is installed in the image.
FROM ubuntu:26.04 AS base
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates libglvnd0 libegl1 libgles2 libgbm1 libxkbcommon0 libopus0 \
    && rm -rf /var/lib/apt/lists/*
ENV NVIDIA_DRIVER_CAPABILITIES=all \
    XDG_RUNTIME_DIR=/run/cha

FROM base AS dev
RUN apt-get update && apt-get install -y --no-install-recommends \
      curl gnupg git build-essential pkg-config clang libclang-dev cmake \
      libgbm-dev libxkbcommon-dev libegl-dev libdrm-dev libopus-dev \
      fonts-dejavu-core procps \
    && rm -rf /var/lib/apt/lists/*
# Google Chrome, the P1.3 bench client.
RUN install -d -m 0755 /etc/apt/keyrings \
 && curl -fsSL https://dl.google.com/linux/linux_signing_key.pub | gpg --dearmor -o /etc/apt/keyrings/google.gpg \
 && echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/google.gpg] https://dl.google.com/linux/chrome/deb/ stable main" \
      > /etc/apt/sources.list.d/google-chrome.list \
 && apt-get update && apt-get install -y --no-install-recommends google-chrome-stable \
 && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.93 --component clippy,rustfmt
ENV PATH=/root/.cargo/bin:$PATH \
    CARGO_TARGET_DIR=/target
WORKDIR /src

FROM dev AS build
COPY . .
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/target \
    cargo build --release --locked -p cha-streamer \
 && cp /target/release/cha-streamer /usr/local/bin/cha-streamer

FROM base AS runtime
COPY --from=build /usr/local/bin/cha-streamer /usr/local/bin/cha-streamer
ENTRYPOINT ["cha-streamer"]
