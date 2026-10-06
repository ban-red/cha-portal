# Steam in Big Picture (the gamepad UI), inside gamescope nested in our
# compositor (plan P2.1). 64-bit games render on the GPU through CDI; 32-bit
# GL falls back to Mesa unless the host's CDI spec carries NVIDIA's 32-bit
# libraries. gamescope keeps Steam's clipboard in its own X server: the helper
# that XFCE uses bridges it (crates/cha-x11-clipboard). steam-status shows the
# first launch's download to the page; steam-library lists the library the node
# shares across users (CHA_SHARED_DIR) among Steam's library folders;
# steam-per-user-dirs stops a start whose per-user folders over it aren't mounted;
# steam-run keeps gamescope's output and reports its crash.
#
# The performance overlay is MangoHud's mangoapp, which gamescope starts
# (start-steam's --mangoapp) and which draws in gamescope's picture. Two parts
# are built here rather than installed:
# - gamescope is Ubuntu's own source with one patch
#   (gamescope-wayland-mangoapp.patch): nested in a Wayland compositor,
#   gamescope never tells mangoapp when a frame was shown, so for Steam's own UI
#   and any vsynced game the overlay never draws;
# - mangoapp is upstream MangoHud's, because Ubuntu's package is built without
#   NVML and shows no NVIDIA GPU statistics (NVML is loaded at run time, and the
#   node's CDI spec brings libnvidia-ml).
#
# Steam makes the virtual Xbox pad it gives games through /dev/uinput, which
# the app doesn't get (a game could make keyboards and mice on the node with
# it): libcha-uinput.so, loaded into Steam by start-steam's LD_PRELOAD, fakes
# that file and asks the streamer's broker for the device (uinput-shim/).
FROM rust:1.93-trixie AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p cha-x11-clipboard \
 && cp target/release/cha-x11-clipboard /usr/local/bin/

# Ubuntu's gamescope with the patch, as a package that replaces the stock one.
FROM base AS gamescope
USER root
RUN sed -i 's/^Types: deb$/Types: deb deb-src/; s/^Components: main.*/Components: main restricted universe multiverse/' /etc/apt/sources.list.d/ubuntu.sources
COPY images/steam/gamescope-wayland-mangoapp.patch /tmp/
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    rm -f /etc/apt/apt.conf.d/docker-clean \
 && apt-get update && apt-get install -y --no-install-recommends build-essential devscripts \
 && apt-get build-dep -y gamescope
WORKDIR /build
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get source gamescope \
 && cd gamescope-*/ \
 && patch -p1 < /tmp/gamescope-wayland-mangoapp.patch \
 && DEBFULLNAME=cha DEBEMAIL=cha@localhost dch --local +cha "Wayland backend: tell mangoapp when a frame was shown" \
 && DEB_BUILD_OPTIONS="parallel=$(nproc) nocheck" dpkg-buildpackage -b -uc -us \
 && mkdir /out && cp ../gamescope_*+cha*_amd64.deb /out/

# mangoapp from upstream MangoHud, pinned to the v0.8.2 tag and its commit.
FROM base AS mangoapp
USER root
RUN sed -i 's/^Types: deb$/Types: deb deb-src/; s/^Components: main.*/Components: main restricted universe multiverse/' /etc/apt/sources.list.d/ubuntu.sources
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    rm -f /etc/apt/apt.conf.d/docker-clean \
 && apt-get update && apt-get build-dep -y mangohud \
 && apt-get install -y --no-install-recommends git ca-certificates meson ninja-build \
      glslang-tools python3-mako libglfw3-dev libx11-dev libwayland-dev libxkbcommon-dev \
      libdbus-1-dev nlohmann-json3-dev libspdlog-dev libfmt-dev pkg-config
WORKDIR /src
RUN git clone --depth 1 --branch v0.8.2 https://github.com/flightlessmango/MangoHud . \
 && test "$(git rev-parse HEAD)" = a37b007e8018738270c9282c3a53b0ed7e786bcf \
 # GCC 15 no longer pulls <sstream> in through other headers.
 && sed -i '1i #include <sstream>' src/gpu_fdinfo.cpp \
 && meson setup build --prefix=/usr -Dmangoapp=true -Dmangohudctl=true \
      -Dwith_xnvctrl=disabled -Dtests=disabled -Dmangoplot=disabled -Duse_system_spdlog=enabled \
 && ninja -C build \
 && install -D build/src/mangoapp /out/usr/bin/mangoapp \
 # The packages that own the libraries mangoapp loads, for the final stage.
 && ldd /out/usr/bin/mangoapp | awk '/=> \//{print $3}' \
    | xargs dpkg-query -S | cut -d: -f1 | sort -u > /out/runtime-packages

# The /dev/uinput shim, for the i386 client and the 64-bit helpers that load it
# too, in /opt/cha/lib/<triplet>/ where the loader's $LIB looks.
FROM base AS uinput-shim
USER root
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    rm -f /etc/apt/apt.conf.d/docker-clean \
 && apt-get update && apt-get install -y --no-install-recommends gcc gcc-multilib make libc6-dev
WORKDIR /src
COPY images/steam/uinput-shim/ .
RUN make OUT=/out/opt/cha

FROM base
USER root
COPY --from=gamescope /out/gamescope_*.deb /tmp/
COPY --from=mangoapp /out/runtime-packages /tmp/
RUN dpkg --add-architecture i386 \
 && sed -i 's/^Components: main.*/Components: main restricted universe multiverse/' /etc/apt/sources.list.d/ubuntu.sources \
 && apt-get update && apt-get install -y --no-install-recommends \
      curl xwayland dbus dbus-user-session \
      libgl1:i386 libegl1:i386 libgbm1:i386 libvulkan1:i386 libgl1-mesa-dri:i386 mesa-vulkan-drivers:i386 \
      libpulse0:i386 libxkbcommon0:i386 \
      xdg-user-dirs xdg-utils file xz-utils lsof pciutils python3 zenity \
 && curl -fsSL -o /tmp/steam.deb https://repo.steampowered.com/steam/archive/stable/steam_latest.deb \
 && apt-get install -y /tmp/steam.deb \
 # The patched gamescope (its version carries +cha), held so a later apt-get
 # upgrade doesn't put Ubuntu's back, and what mangoapp loads.
 && apt-get install -y --no-install-recommends /tmp/gamescope_*.deb $(cat /tmp/runtime-packages) \
 && apt-mark hold gamescope \
 && apt-get update && apt-get install -y --no-install-recommends steam-libs-amd64 steam-libs-i386 \
 && rm -rf /tmp/steam.deb /tmp/gamescope_*.deb /tmp/runtime-packages /var/lib/apt/lists/* \
 # The launcher would run steamdeps, which offers to apt-install anything
 # missing and waits in an xterm for a key press: everything is installed.
 && rm /usr/bin/steamdeps \
 # pressure-vessel needs a real bubblewrap (its own): no stand-in. This is
 # what the `steam` profile's cha-sandbox AppArmor profile is for.
 && rm /usr/local/bin/bwrap
# gamescope's Xwayland runs unprivileged and can't create the socket dirs.
RUN install -d -m 1777 /tmp/.X11-unix /tmp/.ICE-unix
COPY --from=build /usr/local/bin/cha-x11-clipboard /usr/local/bin/
COPY --from=mangoapp /out/usr/bin/mangoapp /usr/local/bin/
COPY --from=uinput-shim /out/opt/cha /opt/cha
COPY images/steam/start-steam images/steam/steam-touch-mode images/steam/steam-focus images/steam/steam-status images/steam/steam-library images/steam/steam-per-user-dirs images/steam/steam-run /usr/local/bin/
# Debian puts gamescope and steam in /usr/games.
ENV PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/games
USER cha
LABEL sh.cha.template="steam"
CMD ["start-steam"]
