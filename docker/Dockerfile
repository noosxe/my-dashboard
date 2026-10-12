# Reproducible local verification environment (docs/0071).
#
# Env-only image: the repo is mounted at /work by scripts/verify — never
# COPY'd in — so every run verifies the working tree as it stands, and the
# image itself almost never changes.
#
# The base pin is deliberate (docs/0071 D2): bump the tag, digest and
# RUST_TOOLCHAIN in .github/workflows/ci.yml together, and recreate the
# cargo cache volume (docs/0071 D6) in the same change.
FROM rust:1.98.0@sha256:620dbcd124499c59e2406d3741574b5c5838cf9eb9656f0c3a03948f79b02959

# GUI dev stack — keep in lockstep with the apt list in
# .github/workflows/ci.yml (docs/0027 D1: only what the base image does
# not ship). Deliberately minus fonts-ibm-plex: that is an Ubuntu
# package name and the rust base is Debian trixie, which ships no
# equivalent — IBM Plex is vendored from the upstream release instead
# (next RUN). The additions beyond CI's list are the headless
# verification stack: sway + dbus + at-spi2-core for dashboard-debug
# sessions, grim for docs/0065 critique screenshots, xvfb + cmake for
# hosted-runner build-baseline parity (docs/0027 §2), dejavu as glyph
# fallback coverage under the pixman renderer, fontconfig + unzip for
# the Plex vendoring below.
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      pkg-config \
      libwayland-dev libvulkan-dev libxcb1-dev libxkbcommon-dev \
      libxkbcommon-x11-dev libatk1.0-dev libfontconfig1-dev libglib2.0-dev \
      libgtk-3-dev \
      fonts-dejavu-core \
      fontconfig unzip \
      sway dbus at-spi2-core grim \
      xvfb cmake \
      jq \
 && rm -rf /var/lib/apt/lists/*

# IBM Plex (docs/0071 D3): vendored from the upstream v6.4.0 release —
# the version Ubuntu noble packages for CI — because trixie has no
# equivalent package. Both zips pinned by sha256; only the TTF cuts the
# app references (Sans for UI, Mono for code) ship. A hash mismatch or
# a moved release fails the build here, loudly.
RUN mkdir -p /tmp/plex /usr/local/share/fonts/ibm-plex \
 && curl -fsSL -o /tmp/plex/sans.zip \
      https://github.com/IBM/plex/releases/download/v6.4.0/IBM-Plex-Sans.zip \
 && curl -fsSL -o /tmp/plex/mono.zip \
      https://github.com/IBM/plex/releases/download/v6.4.0/IBM-Plex-Mono.zip \
 && printf '%s  /tmp/plex/sans.zip\n%s  /tmp/plex/mono.zip\n' \
      e9cac220220f66e8c3b903320a1ebd0e0bcfbfd216772879d33cc71de94570ab \
      b3b47ba5795ea36abe1afc9fdddab228ba8e2b49feb5fc8aa6de65d3170fa8f2 \
      | sha256sum -c - \
 && unzip -q -j /tmp/plex/sans.zip '*.ttf' -d /usr/local/share/fonts/ibm-plex/ \
 && unzip -q -j /tmp/plex/mono.zip '*.ttf' -d /usr/local/share/fonts/ibm-plex/ \
 && fc-cache -f >/dev/null \
 && rm -rf /tmp/plex
# at-spi-bus-launcher lives in /usr/libexec on bookworm but is exec'd by
# bare name — the shim the #523 receipt runs discovered by failure. The
# test fails the build if the path ever moves (fail-safe, like CI's
# sudo guard), instead of producing an image whose a11y silently breaks.
RUN test -x /usr/libexec/at-spi-bus-launcher \
 && ln -sf /usr/libexec/at-spi-bus-launcher /usr/local/bin/at-spi-bus-launcher

# CARGO_TARGET_DIR keeps build output off the macOS bind mount (the
# #523 runs paid a visible tax writing target/ through osxfs before the
# named volume); /tmp/target is backed by the gpui-diff-target volume
# mounted by scripts/verify. XDG_RUNTIME_DIR is created 0700 by the
# entrypoint.
ENV CARGO_TARGET_DIR=/tmp/target \
    XDG_RUNTIME_DIR=/tmp/xdg

COPY entrypoint.sh /usr/local/bin/verify-entrypoint
RUN chmod +x /usr/local/bin/verify-entrypoint

WORKDIR /work
ENTRYPOINT ["/usr/local/bin/verify-entrypoint"]
CMD ["bash"]
