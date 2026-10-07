# The Moonlight gateway: streams a Sunshine or Apollo host to the browser
# player, in place of cha-streamer (ADR 0008). Build context: the repository root.
#   docker build -f deploy/gateway/Dockerfile -t cha/gateway:dev .
# The node agent starts it; it needs no GPU.

FROM rust:1.93-trixie AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p cha-gateway \
 && cp target/release/cha-gateway /usr/local/bin/cha-gateway

FROM ubuntu:26.04
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/cha-gateway /usr/local/bin/cha-gateway
ENTRYPOINT ["cha-gateway"]
