#!/bin/bash
# Fail when the Rust toolchain pins disagree (docs/0027 D4).
#
# The dev-shell flake is the reference toolchain; CI (rustup, nix-free by
# owner decision), the verification image (docs/0071 D2) and the workspace
# `rust-version` pin the same release. A bump changes all four in one PR —
# this check is what makes "in lockstep" more than a comment. Runs in the
# CI lint job; needs only grep/sed, no toolchain.
set -euo pipefail

cd "$(dirname "$0")/.."

flake=$(sed -n 's/.*rust-bin\.stable\."\([0-9.]*\)".*/\1/p' flake.nix)
ci=$(sed -n 's/^  RUST_TOOLCHAIN: "\([0-9.]*\)"$/\1/p' .github/workflows/ci.yml)
docker=$(sed -n 's/^FROM rust:\([0-9.]*\)@.*/\1/p' docker/Dockerfile)
cargo=$(sed -n 's/^rust-version = "\([0-9.]*\)"$/\1/p' Cargo.toml)

printf '%-34s %s\n' \
  "flake.nix (rust-bin.stable)" "${flake:-<missing>}" \
  ".github/workflows/ci.yml (env)" "${ci:-<missing>}" \
  "docker/Dockerfile (FROM rust)" "${docker:-<missing>}" \
  "Cargo.toml (rust-version)" "${cargo:-<missing>}"

if [ -z "$flake" ] || [ "$flake" != "$ci" ] || [ "$flake" != "$docker" ] || [ "$flake" != "$cargo" ]; then
  echo "toolchain pins disagree — bump all four together (docs/0027 D4)" >&2
  exit 1
fi
echo "toolchain pins agree: $flake"
