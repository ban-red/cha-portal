# The base of every Cha environment image (see images/README.md).
# The NVIDIA driver (EGL, GL, Vulkan, GBM) comes from CDI at run time.
# Pinned by digest (Dependabot bumps it, see .github/dependabot.yml); a change
# here changes images/base/Dockerfile, so the published inputs hash follows.
FROM ubuntu:26.04@sha256:f144425ff09be612d6d9ad965196e9cdc23dae1f42110a8a11a3e9a8198759f7
ENV DEBIAN_FRONTEND=noninteractive
# Every apt RUN in images/ mounts apt's package and list caches instead of
# writing them into a layer, so no apt lists end up in an image and a rebuild
# fetches less. Ubuntu's docker-clean hook, which deletes downloaded packages
# after each install, is set aside for this RUN only: it stays in the image,
# so an image built on this one without cache mounts (a third party's) still
# leaves no packages in its layers. In our images built on it, the hook
# empties the package cache mount after an install; the lists stay cached.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt/lists,sharing=locked \
    mv /etc/apt/apt.conf.d/docker-clean /tmp/docker-clean \
 && apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates locales tzdata \
      libglvnd0 libegl1 libgl1 libgles2 libvulkan1 libgbm1 \
      libwayland-client0 libwayland-cursor0 libwayland-egl1 libxkbcommon0 \
      libpulse0 \
      fonts-dejavu-core fonts-noto-core fonts-noto-color-emoji dmz-cursor-theme \
 && sed -i 's/^# *en_US.UTF-8/en_US.UTF-8/' /etc/locale.gen && locale-gen \
 && mv /tmp/docker-clean /etc/apt/apt.conf.d/docker-clean
# The image's uid 1000 user becomes ours.
RUN usermod -l cha -d /home/cha -m ubuntu && groupmod -n cha ubuntu
# Desktop sessions run unprivileged and can't create the X11 and ICE socket dirs.
RUN install -d -m 1777 /tmp/.X11-unix /tmp/.ICE-unix
COPY images/base/cha-run /usr/local/bin/cha-run
COPY images/base/bwrap-shim /usr/local/bin/bwrap
ENV LANG=en_US.UTF-8 \
    HOME=/home/cha \
    XDG_RUNTIME_DIR=/run/cha \
    WAYLAND_DISPLAY=wayland-0 \
    PULSE_SERVER=unix:/run/cha/pulse/native \
    SDL_JOYSTICK_DISABLE_UDEV=1 \
    XCURSOR_THEME=DMZ-White \
    NVIDIA_DRIVER_CAPABILITIES=all
USER cha
WORKDIR /home/cha
ENTRYPOINT ["cha-run"]
