#!/usr/bin/env bash
# System build dependencies for mattermost-rs on Ubuntu 24.04.
#
# Kept in one file so the CI runner, the README and a fresh workstation cannot
# drift apart. These are BUILD dependencies; the .deb's runtime dependencies
# come from cargo-deb's $auto resolution.
#
# libgtk-4-dev pulls in cairo, pango, gdk-pixbuf, graphene, epoxy, vulkan and
# the X11 and Wayland dev packages, so they are not listed individually.
set -euo pipefail

PKGS=(
  build-essential
  pkg-config
  libgtk-4-dev
  libadwaita-1-dev
  libgtksourceview-5-dev   # syntax-highlighted code blocks in messages
)

# musl-tools builds the static mm-headless binary the Docker lab injects.
PKGS+=(musl-tools)

echo "installing: ${PKGS[*]}"
sudo apt-get update
sudo apt-get install -y --no-install-recommends "${PKGS[@]}"
