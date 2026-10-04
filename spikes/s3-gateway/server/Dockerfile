# Spike S3 gateway. Build from the repository root:
#   docker build -f spikes/s3-gateway/server/Dockerfile -t cha/s3-gateway .
# It runs next to Wolf with host networking (see ../compose.yaml).
FROM rust:1.93-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p s3-gateway

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/s3-gateway /usr/local/bin/s3-gateway
EXPOSE 4490/tcp 4491/udp
ENTRYPOINT ["s3-gateway"]
