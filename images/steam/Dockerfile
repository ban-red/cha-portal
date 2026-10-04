# Steam in Big Picture (the gamepad UI), inside gamescope nested in our
# compositor (plan P2.1). 64-bit games render on the GPU through CDI; 32-bit
# GL falls back to Mesa unless the host's CDI spec carries NVIDIA's 32-bit
# libraries.
FROM base
USER root
RUN dpkg --add-architecture i386 \
 && sed -i 's/^Components: main.*/Components: main restricted universe multiverse/' /etc/apt/sources.list.d/ubuntu.sources \
 && apt-get update && apt-get install -y --no-install-recommends \
      curl gamescope xwayland dbus dbus-user-session \
      libgl1:i386 libegl1:i386 libgbm1:i386 libvulkan1:i386 libgl1-mesa-dri:i386 mesa-vulkan-drivers:i386 \
      libpulse0:i386 libxkbcommon0:i386 \
      xdg-user-dirs xdg-utils file xz-utils lsof pciutils python3 zenity \
 && curl -fsSL -o /tmp/steam.deb https://repo.steampowered.com/steam/archive/stable/steam_latest.deb \
 && apt-get install -y /tmp/steam.deb \
 && apt-get update && apt-get install -y --no-install-recommends steam-libs-amd64 steam-libs-i386 \
 && rm -rf /tmp/steam.deb /var/lib/apt/lists/* \
 # The launcher would run steamdeps, which offers to apt-install anything
 # missing and waits in an xterm for a key press: everything is installed.
 && rm /usr/bin/steamdeps \
 # pressure-vessel needs a real bubblewrap (its own): no stand-in. This is
 # what the `steam` profile's cha-sandbox AppArmor profile is for.
 && rm /usr/local/bin/bwrap
# gamescope's Xwayland runs unprivileged and can't create the socket dirs.
RUN install -d -m 1777 /tmp/.X11-unix /tmp/.ICE-unix
COPY images/steam/start-steam /usr/local/bin/start-steam
# Debian puts gamescope and steam in /usr/games.
ENV PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/games
USER cha
LABEL sh.cha.template="steam"
CMD ["start-steam"]
