#!/usr/bin/env bash
# Canonical entry into the containerized verification environment
# (docs/0071). Runs any command inside the Linux verification image with
# the repo bind-mounted at /work and the warm cargo caches attached:
#
#   scripts/verify.sh cargo fmt --all --check
#   scripts/verify.sh cargo clippy --workspace --all-targets -- -D warnings
#   scripts/verify.sh cargo test --workspace
#   scripts/verify.sh bash                  # interactive shell in the env
#
# Opt-in GPU mode, for performance measurements only (docs/0071 D10):
#
#   scripts/verify.sh --gpu auto <cmd…>             # first Mesa AMD render node
#   scripts/verify.sh --gpu /dev/dri/renderD128 <cmd…>
#
# The default mode stays on software rendering so goldens reproduce.
#
# The receipt sequence (docs/0065, docs/0010 §3) runs through the same
# door — see docs/0071 §4 for the canonical commands.
set -euo pipefail
cd "$(dirname "$0")/.."

IMAGE=my-dashboard-verify
GPU_ARGS=()
TARGET=verify

if [ "${1:-}" = "--gpu" ]; then
  [ $# -ge 3 ] || { echo "usage: verify.sh --gpu <render-node|auto> <cmd…>" >&2; exit 2; }
  NODE=$2
  shift 2
  if [ "$NODE" = auto ]; then
    # Vendor 0x1002 is AMD; the card order of render nodes varies per boot.
    NODE=
    for v in /sys/class/drm/renderD*/device/vendor; do
      if [ "$(cat "$v")" = 0x1002 ]; then
        NODE=/dev/dri/$(basename "$(dirname "$(dirname "$v")")")
        break
      fi
    done
    [ -n "$NODE" ] || { echo "verify.sh: no AMD render node found" >&2; exit 1; }
  fi
  [ -c "$NODE" ] || { echo "verify.sh: $NODE is not a render node" >&2; exit 1; }
  # The container user must be able to open the node: join its group.
  GPU_ARGS=(--device "$NODE" --group-add "$(stat -c %g "$NODE")"
    -e WLR_RENDERER=gles2 -e WLR_RENDER_DRM_DEVICE="$NODE")
  TARGET=verify-gpu
  IMAGE=my-dashboard-verify-gpu
  echo "verify.sh: GPU mode on $NODE (not for goldens or receipts)" >&2
fi

# Rebuild only when the Dockerfile changed (content-hash layers make the
# no-op case fast); id to stderr so stdout stays clean for pipelines.
docker build -q --target "$TARGET" -t "$IMAGE" "$(dirname "$0")/../docker" 1>&2

TTY=
[ -t 0 ] && TTY=-t

exec docker run --rm -i $TTY ${GPU_ARGS[@]+"${GPU_ARGS[@]}"} \
  -v "$PWD":/work -w /work \
  -v gpui-diff-cargo-cache:/usr/local/cargo \
  -v gpui-diff-target:/tmp/target \
  "$IMAGE" "$@"
