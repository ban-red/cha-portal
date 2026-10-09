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

# cloudflared, for internet share links (ADR 0022): a pinned release from
# GitHub, checked against its SHA-256 (the digests are on the release page).
# To move to a newer release, change the version and both digests together.
FROM ubuntu:26.04 AS cloudflared
ARG TARGETARCH
ARG CLOUDFLARED_VERSION=2026.10.0
ARG CLOUDFLARED_SHA256_AMD64=d33ff2d14475178d2012c2c56beba87389ac5ded27649519f198a7d3134a99db
ARG CLOUDFLARED_SHA256_ARM64=e6422b9d4f72d3194bc5a38676f13667c06666523217b842a877d72a80b5ac08
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && case "$TARGETARCH" in \
      amd64) sha="$CLOUDFLARED_SHA256_AMD64" ;; \
      arm64) sha="$CLOUDFLARED_SHA256_ARM64" ;; \
      *) echo "no cloudflared build for $TARGETARCH" >&2; exit 1 ;; \
    esac \
 && curl -fsSL -o /cloudflared \
      "https://github.com/cloudflare/cloudflared/releases/download/${CLOUDFLARED_VERSION}/cloudflared-linux-${TARGETARCH}" \
 && echo "${sha}  /cloudflared" | sha256sum -c - \
 && chmod 0755 /cloudflared

FROM ubuntu:26.04
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --home-dir /var/lib/cha --no-create-home cha \
 && install -d -m 700 -o cha -g cha /var/lib/cha
COPY --from=build /usr/local/bin/cha-control /usr/local/bin/cha-control
COPY --from=cloudflared /cloudflared /usr/local/bin/cloudflared
COPY --from=web /src/web/apps/portal/dist /usr/share/cha/web
ENV CHA_LISTEN=0.0.0.0:7676 \
    CHA_DATABASE=/var/lib/cha/cha.db \
    CHA_WEB_DIR=/usr/share/cha/web
USER cha
# The database (accounts, nodes, environments, the audit log, the media key),
# in a directory only the portal's user can enter.
VOLUME /var/lib/cha
# 7676 is the portal. Internet share links add a second listener, 7680
# (CHA_GUEST_LISTEN), on loopback only: the Cloudflare Tunnel connects to it
# from inside the container.
EXPOSE 7676
ENTRYPOINT ["cha-control"]
