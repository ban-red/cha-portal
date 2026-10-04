# The Cha Node agent. Build context: the repository root.
#   docker compose -f deploy/node/compose.yaml build

FROM rust:1.93-trixie AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p cha-node \
 && cp target/release/cha-node /usr/local/bin/cha-node

FROM ubuntu:26.04
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/cha-node /usr/local/bin/cha-node
# The node's identity, including its private key.
VOLUME /var/lib/cha-node
ENTRYPOINT ["cha-node"]
