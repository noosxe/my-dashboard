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
# The receipt sequence (docs/0065, docs/0010 §3) runs through the same
# door — see docs/0071 §4 for the canonical commands.
set -euo pipefail
cd "$(dirname "$0")/.."

IMAGE=my-dashboard-verify

# Rebuild only when the Dockerfile changed (content-hash layers make the
# no-op case fast); id to stderr so stdout stays clean for pipelines.
docker build -q -t "$IMAGE" "$(dirname "$0")/../docker" 1>&2

TTY=
[ -t 0 ] && TTY=-t

exec docker run --rm -i $TTY \
  -v "$PWD":/work -w /work \
  -v gpui-diff-cargo-cache:/usr/local/cargo \
  -v gpui-diff-target:/tmp/target \
  "$IMAGE" "$@"
