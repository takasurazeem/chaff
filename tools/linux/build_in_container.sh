#!/usr/bin/env bash
# Build the Chaff GUI on a Linux host without needing the host's sudo.
#
# Tauri needs webkit2gtk-4.1, javascriptcoregtk-4.1, libsoup3 and librsvg development
# packages. Installing them on the host needs root. A distrobox container has its own
# root, so they install there, and the application runs there too — which is also what
# makes the library versions consistent at runtime.
#
# Undo everything with:  distrobox rm -f chaff-build
#
# Usage:  tools/linux/build_in_container.sh [--run]
set -euo pipefail

CONTAINER="${CHAFF_CONTAINER:-chaff-build}"
IMAGE="registry.fedoraproject.org/fedora-toolbox:latest"
SRC="${CHAFF_SRC:-$HOME/chaff}"

if ! distrobox list 2>/dev/null | grep -q "$CONTAINER"; then
  echo "creating $CONTAINER from $IMAGE"
  distrobox create --name "$CONTAINER" --image "$IMAGE" --yes
fi

echo "=== ensuring build dependencies ==="
distrobox enter "$CONTAINER" -- bash -lc '
set -e
missing=0
for p in webkit2gtk-4.1 javascriptcoregtk-4.1 libsoup-3.0 librsvg-2.0; do
  pkg-config --exists "$p" || missing=1
done
# Rust is installed in the container rather than borrowed from the host: the host toolchain
# is a package for a different Fedora release, and a compiler from one release linking
# against another release'"'"'s glibc is a class of failure worth not having.
command -v cargo >/dev/null || missing=1
if [ "$missing" = 1 ]; then
  sudo dnf install -y --setopt=install_weak_deps=False \
    webkit2gtk4.1-devel javascriptcoregtk4.1-devel libsoup3-devel librsvg2-devel \
    openssl-devel gtk3-devel libappindicator-gtk3-devel rust cargo
fi
echo "rustc: $(rustc --version 2>/dev/null || echo MISSING)"
'

echo "=== building the frontend ==="
# The release binary embeds ../dist. Building it without the frontend present produces a
# binary that starts and shows an empty window.
( cd "$SRC" && pnpm build )

echo "=== building ==="
distrobox enter "$CONTAINER" -- bash -lc "
set -e
cd '$SRC'
# **--features custom-protocol is not optional.** Without it the binary serves devUrl
# instead of the embedded frontend and fails with 'Could not connect to localhost'.
cargo build --release -p chaff --features custom-protocol
ls -la target/release/chaff
"

if [ "${1:-}" = "--run" ]; then
  echo "=== launching (GUI forwards to the host display) ==="
  distrobox enter "$CONTAINER" -- bash -lc "cd '$SRC' && ./target/release/chaff"
fi
