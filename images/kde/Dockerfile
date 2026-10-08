# KDE Plasma, nested: KWin runs as a Wayland client of our compositor (its
# window is the whole picture), with its own Xwayland, and Plasma inside it.
# No systemd: Plasma starts the session's services itself.
FROM base
USER root
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt/lists,sharing=locked \
    sed -i 's/^Components: main.*/Components: main restricted universe multiverse/' /etc/apt/sources.list.d/ubuntu.sources \
 && apt-get update && apt-get install -y --no-install-recommends \
      kwin-wayland plasma-workspace plasma-desktop plasma-pa xdg-desktop-portal-kde \
      xwayland dbus dbus-user-session \
      konsole dolphin systemsettings kde-cli-tools \
      breeze breeze-icon-theme fonts-noto-ui-core
# kwin_wayland carries a file capability (CAP_SYS_NICE, for real-time
# scheduling), which a container without capabilities refuses to exec: drop it.
RUN setcap -r /usr/bin/kwin_wayland
COPY images/kde/start-plasma images/kde/kde-clean-stale /usr/local/bin/
USER cha
LABEL sh.cha.template="kde"
CMD ["start-plasma"]
