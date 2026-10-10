# Steam in Big Picture (the gamepad UI), inside gamescope nested in our
# compositor (plan P2.1), or on an XFCE desktop (steam-session switches between
# the two, as SteamOS does between Gaming Mode and Desktop Mode). 64-bit games
# render on the GPU through CDI; 32-bit GL falls back to Mesa unless the host's
# CDI spec carries NVIDIA's 32-bit libraries. gamescope keeps Steam's clipboard in its own X server: the helper
# that XFCE uses bridges it (crates/cha-x11-clipboard). steam-status shows the
# first launch's download to the page; steam-library lists the library the node
# shares across users (CHA_SHARED_DIR) among Steam's library folders;
# steam-drives gives each Proton prefix a drive letter for it (Z:\ shows the
# game's tmpfs, not the library's free space);
# steam-per-user-dirs stops a start whose per-user folders over it aren't mounted;
# steam-run keeps a session's output and reports its crash; steam-session runs
# the sessions (steam-gaming, steam-desktop) and steamos-session-select, which
# Steam's "Switch to Desktop" calls, asks it to switch.
#
# The performance overlay is MangoHud's mangoapp, which gamescope starts
# (steam-gaming's --mangoapp) and which draws in gamescope's picture. Two parts
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
# it): libcha-uinput.so, loaded into Steam by steam-gaming's and
# steam-desktop-client's LD_PRELOAD, fakes that file and asks the streamer's
# broker for the device (uinput-shim/).
FROM rust:1.93-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
# Every workspace member's manifest and sources must be there for --locked.
COPY spikes/ spikes/
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
# The final stage is five layers, ordered by how often they change, so a new
# gamescope patch or mangoapp build doesn't make a node pull the multi-hundred
# megabyte i386 libraries, Steam's runtime libraries or XFCE again.
# 1. The i386 architecture and the 32-bit Mesa that games fall back to.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt/lists,sharing=locked \
    dpkg --add-architecture i386 \
 && sed -i 's/^Components: main.*/Components: main restricted universe multiverse/' /etc/apt/sources.list.d/ubuntu.sources \
 && apt-get update && apt-get install -y --no-install-recommends \
      curl xwayland dbus dbus-user-session \
      libgl1:i386 libegl1:i386 libgbm1:i386 libvulkan1:i386 libgl1-mesa-dri:i386 mesa-vulkan-drivers:i386 \
      libpulse0:i386 libxkbcommon0:i386 \
      xdg-user-dirs xdg-utils file xz-utils lsof pciutils python3 zenity
# 2. Valve's Steam package, which adds Valve's apt repository (the second
# `apt-get update` reads it), and the runtime libraries from there.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt/lists,sharing=locked \
    apt-get update \
 && curl -fsSL -o /tmp/steam.deb https://repo.steampowered.com/steam/archive/stable/steam_latest.deb \
 && apt-get install -y /tmp/steam.deb \
 && apt-get update && apt-get install -y --no-install-recommends steam-libs-amd64 steam-libs-i386 \
 && rm -f /tmp/steam.deb
# 3. The XFCE desktop of desktop mode (as images/xfce), without the elementary
# icon theme (~70 MB). gio and gvfs's metadata daemon mark the "Return to Gaming
# Mode" launcher trusted (steam-desktop).
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt/lists,sharing=locked \
    apt-get update && apt-get install -y --no-install-recommends \
      xfce4-session xfwm4 xfce4-panel xfdesktop4 xfce4-settings xfconf thunar xfce4-terminal \
      adwaita-icon-theme dbus-x11 libglib2.0-bin gvfs
# 4. Desktop mode's everyday tools, so the desktop isn't bare: Firefox (Mozilla's
# own .deb, as images/firefox: Ubuntu's is a snap), the system's default
# browser (xdg-open, Steam's links and XFCE's "Web Browser" launcher all ask
# for `www-browser`, which had no program behind it), a text editor, an image
# viewer, an archive manager, a task manager and screenshots, and command-line
# tools for looking around (htop, nvtop, glxinfo, vulkaninfo, git, jq).
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt/lists,sharing=locked \
    install -d -m 0755 /etc/apt/keyrings \
 && curl -fsSL https://packages.mozilla.org/apt/repo-signing-key.gpg -o /etc/apt/keyrings/packages.mozilla.org.asc \
 && echo "deb [signed-by=/etc/apt/keyrings/packages.mozilla.org.asc] https://packages.mozilla.org/apt mozilla main" \
      > /etc/apt/sources.list.d/mozilla.list \
 && printf 'Package: *\nPin: origin packages.mozilla.org\nPin-Priority: 1000\n' > /etc/apt/preferences.d/mozilla \
 && apt-get update && apt-get install -y --no-install-recommends \
      firefox mousepad ristretto file-roller xfce4-taskmanager xfce4-screenshooter \
      nano htop nvtop less wget unzip zip jq git iproute2 iputils-ping bash-completion \
      mesa-utils vulkan-tools \
 && for alt in www-browser x-www-browser gnome-www-browser; do \
      update-alternatives --install /usr/bin/$alt $alt /usr/bin/firefox 200 \
   && update-alternatives --set $alt /usr/bin/firefox; done
COPY images/firefox/policies.json /etc/firefox/policies/policies.json
COPY images/steam/desktop/helpers.rc /etc/xdg/xfce4/helpers.rc
COPY images/steam/desktop/mimeapps.list /etc/xdg/mimeapps.list
# 5. What this repository builds or patches.
COPY --from=gamescope /out/gamescope_*.deb /tmp/
COPY --from=mangoapp /out/runtime-packages /tmp/
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt/lists,sharing=locked \
    apt-get update \
 # The patched gamescope (its version carries +cha), held so a later apt-get
 # upgrade doesn't put Ubuntu's back, and what mangoapp loads.
 && apt-get install -y --no-install-recommends /tmp/gamescope_*.deb $(cat /tmp/runtime-packages) \
 && apt-mark hold gamescope \
 && rm -f /tmp/gamescope_*.deb /tmp/runtime-packages \
 # The launcher would run steamdeps, which offers to apt-install anything
 # missing and waits in an xterm for a key press: everything is installed.
 && rm /usr/bin/steamdeps \
 # pressure-vessel needs a real bubblewrap (its own): no stand-in. This is
 # what the `steam` profile's cha-sandbox AppArmor profile is for.
 && rm /usr/local/bin/bwrap
COPY --from=build /usr/local/bin/cha-x11-clipboard /usr/local/bin/
COPY --from=mangoapp /out/usr/bin/mangoapp /usr/local/bin/
COPY --from=uinput-shim /out/opt/cha /opt/cha
COPY images/steam/start-steam images/steam/steam-session images/steam/steam-gaming images/steam/steam-desktop images/steam/steam-desktop-client images/steam/steamos-session-select images/steam/steamos-select-branch images/steam/dbus-send images/steam/steam-touch-mode images/steam/steam-focus images/steam/steam-status images/steam/steam-library images/steam/steam-drives images/steam/steam-per-user-dirs images/steam/steam-oobe-skip images/steam/steam-run /usr/local/bin/
# SteamOS's updater stand-in, at the absolute path Steam runs.
COPY images/steam/steamos-update /usr/bin/steamos-polkit-helpers/steamos-update
COPY images/steam/desktop/cha-return-to-gaming-mode.desktop /usr/share/applications/
COPY images/steam/desktop/cha-steam.desktop /etc/xdg/autostart/
# Debian puts gamescope and steam in /usr/games.
ENV PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/games
USER cha
LABEL sh.cha.template="steam"
CMD ["start-steam"]
