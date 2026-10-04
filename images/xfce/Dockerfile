# The XFCE desktop in rootful Xwayland, and the helper that bridges its
# clipboard to the streamer (crates/cha-x11-clipboard).
FROM rust:1.93-trixie AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p cha-x11-clipboard \
 && cp target/release/cha-x11-clipboard /usr/local/bin/

FROM base
USER root
RUN apt-get update && apt-get install -y --no-install-recommends \
      xwayland dbus-x11 dbus-user-session \
      xfce4-session xfwm4 xfce4-panel xfdesktop4 xfce4-settings xfconf thunar xfce4-terminal \
      elementary-xfce-icon-theme adwaita-icon-theme \
 && rm -rf /var/lib/apt/lists/*
# The session runs unprivileged and can't create the X11 and ICE socket dirs.
RUN install -d -m 1777 /tmp/.X11-unix /tmp/.ICE-unix
COPY --from=build /usr/local/bin/cha-x11-clipboard /usr/local/bin/
COPY images/xfce/start-xfce /usr/local/bin/start-xfce
USER cha
LABEL sh.cha.template="xfce"
CMD ["start-xfce"]
