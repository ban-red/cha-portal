# Spike S1 server. Build from the repository root:
#   docker build -f spikes/s1-browser-pyrowave/server/Dockerfile -t cha/s1-server .
# Run with host networking so UDP goes straight to the NIC:
#   docker run --rm --network host cha/s1-server
FROM rust:1.93-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p s1-server

FROM debian:bookworm-slim
COPY --from=build /src/target/release/s1-server /usr/local/bin/s1-server
EXPOSE 4480/tcp 4433/udp 4434/udp
ENTRYPOINT ["s1-server"]
