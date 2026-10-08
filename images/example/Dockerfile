# A minimal third-party environment image: the foot terminal, a native Wayland
# client, on Cha's base image. See ../../docs/image-spec.md.
#
# Pin the base to the node agent release you target ({version} in the
# catalog's `image`). To build against a local base instead:
#   docker build --build-arg BASE=cha/env-base:dev -t cha/env-example:dev images/example
ARG BASE=ghcr.io/ban-red/cha-env-base:0.2.1
FROM ${BASE}

# The base runs as uid 1000 (`cha`); installing packages needs root.
USER root
RUN apt-get update \
 && apt-get install -y --no-install-recommends foot \
 && rm -rf /var/lib/apt/lists/*

# What the user's home starts as. The node copies /home/cha out of the image
# the first time a user keeps their data, then never again, so only put
# defaults here, never files an image update must replace.
USER cha
RUN mkdir -p /home/cha/.config/foot
COPY --chown=1000:1000 foot.ini /home/cha/.config/foot/foot.ini

# The base's entrypoint (cha-run) waits for the compositor's socket, then runs
# this command.
CMD ["foot"]
