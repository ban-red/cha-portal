# The portal: cha-control serving the API, the nodes' WebSocket and the built
# SPA. Build context: the repository root.
#   docker compose -f deploy/portal/compose.yaml build

FROM oven/bun:1.4.2 AS web
WORKDIR /src
COPY . .
# Type checking is the checks' job (scripts/check.sh); this only bundles.
RUN bun install --frozen-lockfile && cd web/apps/portal && bunx --bun vite build

FROM rust:1.93-trixie AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p cha-control \
 && cp target/release/cha-control /usr/local/bin/cha-control

FROM ubuntu:26.04
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --home-dir /var/lib/cha --no-create-home cha \
 && install -d -m 700 -o cha -g cha /var/lib/cha
COPY --from=build /usr/local/bin/cha-control /usr/local/bin/cha-control
COPY --from=web /src/web/apps/portal/dist /usr/share/cha/web
ENV CHA_LISTEN=0.0.0.0:8080 \
    CHA_DATABASE=/var/lib/cha/cha.db \
    CHA_WEB_DIR=/usr/share/cha/web
USER cha
# The database (accounts, nodes, environments, the audit log, the media key),
# in a directory only the portal's user can enter.
VOLUME /var/lib/cha
EXPOSE 8080
ENTRYPOINT ["cha-control"]
