# The test-pattern environment: our own Wayland client (crates/cha-testpattern).
FROM rust:1.99.1-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
# Every workspace member's manifest and sources must be there for --locked.
COPY spikes/ spikes/
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --profile dist --locked -p cha-testpattern \
 && cp target/dist/cha-testpattern /usr/local/bin/

FROM base
COPY --from=build /usr/local/bin/cha-testpattern /usr/local/bin/
LABEL sh.cha.template="test-pattern"
CMD ["cha-testpattern"]
