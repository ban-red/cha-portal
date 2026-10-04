# Spike S7: the WAN tier's bench (see README.md). The streamer's dev image,
# running the binaries the dev loop builds (its target volume, mounted), plus
# tc. The container has its own network namespace, so netem shapes only it.
FROM cha/streamer-dev:latest
RUN apt-get update && apt-get install -y --no-install-recommends iproute2 \
    && rm -rf /var/lib/apt/lists/*
