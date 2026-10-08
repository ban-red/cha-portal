# Google Chrome as a Wayland client, GPU-rendered.
FROM base
USER root
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt/lists,sharing=locked \
    apt-get update && apt-get install -y --no-install-recommends curl gnupg \
 && install -d -m 0755 /etc/apt/keyrings \
 && curl -fsSL https://dl.google.com/linux/linux_signing_key.pub | gpg --dearmor -o /etc/apt/keyrings/google.gpg \
 && echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/google.gpg] https://dl.google.com/linux/chrome/deb/ stable main" \
      > /etc/apt/sources.list.d/google-chrome.list \
 && apt-get update && apt-get install -y --no-install-recommends google-chrome-stable \
 && apt-get purge -y curl gnupg && apt-get autoremove -y
USER cha
LABEL sh.cha.template="chrome"
CMD ["google-chrome-stable", "--ozone-platform=wayland", "--no-first-run", "--no-default-browser-check", \
     "--password-store=basic", "--force-device-scale-factor=1", "--start-maximized"]
