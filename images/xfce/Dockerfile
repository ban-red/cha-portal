# The XFCE desktop in rootful Xwayland.
FROM base
USER root
RUN apt-get update && apt-get install -y --no-install-recommends \
      xwayland dbus-x11 dbus-user-session \
      xfce4-session xfwm4 xfce4-panel xfdesktop4 xfce4-settings xfconf thunar xfce4-terminal \
      elementary-xfce-icon-theme adwaita-icon-theme \
 && rm -rf /var/lib/apt/lists/*
# The session runs unprivileged and can't create the X11 and ICE socket dirs.
RUN install -d -m 1777 /tmp/.X11-unix /tmp/.ICE-unix
COPY images/xfce/start-xfce /usr/local/bin/start-xfce
USER cha
LABEL sh.cha.template="xfce"
CMD ["start-xfce"]
