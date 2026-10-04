# The test-pattern environment: our own Wayland client (crates/cha-testpattern).
FROM rust:1.93-trixie AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p cha-testpattern \
 && cp target/release/cha-testpattern /usr/local/bin/

FROM base
COPY --from=build /usr/local/bin/cha-testpattern /usr/local/bin/
LABEL sh.cha.template="test-pattern"
CMD ["cha-testpattern"]
