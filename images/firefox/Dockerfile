# Firefox (Mozilla's own .deb; Ubuntu's is a snap) as a Wayland client.
FROM base
USER root
RUN apt-get update && apt-get install -y --no-install-recommends curl \
 && install -d -m 0755 /etc/apt/keyrings \
 && curl -fsSL https://packages.mozilla.org/apt/repo-signing-key.gpg -o /etc/apt/keyrings/packages.mozilla.org.asc \
 && echo "deb [signed-by=/etc/apt/keyrings/packages.mozilla.org.asc] https://packages.mozilla.org/apt mozilla main" \
      > /etc/apt/sources.list.d/mozilla.list \
 && printf 'Package: *\nPin: origin packages.mozilla.org\nPin-Priority: 1000\n' > /etc/apt/preferences.d/mozilla \
 && apt-get update && apt-get install -y --no-install-recommends firefox \
 && apt-get purge -y curl && apt-get autoremove -y && rm -rf /var/lib/apt/lists/*
COPY images/firefox/policies.json /etc/firefox/policies/policies.json
USER cha
ENV MOZ_ENABLE_WAYLAND=1
LABEL sh.cha.template="firefox"
CMD ["firefox"]
