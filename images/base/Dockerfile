# The base of every Cha environment image (see images/README.md).
# The NVIDIA driver (EGL, GL, Vulkan, GBM) comes from CDI at run time.
FROM ubuntu:26.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates locales tzdata \
      libglvnd0 libegl1 libgl1 libgles2 libvulkan1 libgbm1 \
      libwayland-client0 libwayland-cursor0 libwayland-egl1 libxkbcommon0 \
      libpulse0 \
      fonts-dejavu-core fonts-noto-core fonts-noto-color-emoji dmz-cursor-theme \
 && rm -rf /var/lib/apt/lists/* \
 && sed -i 's/^# *en_US.UTF-8/en_US.UTF-8/' /etc/locale.gen && locale-gen
# The image's uid 1000 user becomes ours.
RUN usermod -l cha -d /home/cha -m ubuntu && groupmod -n cha ubuntu
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
