#!/usr/bin/env bash
# Render the Catppuccin themes with Whiskers and install them into the
# app's user themes dir — the manual, non-nix installation path.
#
# The nix side (flake.nix `my-dashboard-catppuccin-themes` +
# nix/hm-module.nix) does the same thing declaratively; this script is
# for machines where the app runs from a dev checkout (docs/0025).
#
# The app watches that dir: save is enough, running windows re-render —
# no restart needed (docs/0025 D1/D5).
#
# Usage:
#   scripts/install-catppuccin-theme.sh               # all four flavors
#   scripts/install-catppuccin-theme.sh mocha latte   # a subset
#   scripts/install-catppuccin-theme.sh --themes-dir D  # explicit target
#
# Dependencies: whiskers >= 2.5 (cargo install catppuccin-whiskers, or
# `nix shell nixpkgs#catppuccin-whiskers`), jq.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
template="$repo_root/nix/catppuccin.tera"
all_flavors="latte frappe macchiato mocha"
flavors=()
themes_dir_override=""

usage() { sed -n '2,16p' "$0"; exit "${1:-0}"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --themes-dir) themes_dir_override="$2"; shift 2 ;;
    -h|--help) usage 0 ;;
    -*) echo "unknown flag: $1" >&2; usage 1 ;;
    *) flavors+=("$1"); shift ;;
  esac
done
[ ${#flavors[@]} -eq 0 ] && flavors=($all_flavors)

for f in "${flavors[@]}"; do
  case " $all_flavors " in
    *" $f "*) ;;
    *) echo "unknown flavor: $f (known: $all_flavors)" >&2; exit 1 ;;
  esac
done

# Same resolution as the app's AppPaths (crates/core/src/paths.rs):
# DASHBOARD_STATE_DIR wins; macOS uses Application Support; everyone
# else gets XDG config home.
resolve_themes_dir() {
  if [ -n "$themes_dir_override" ]; then
    printf '%s\n' "$themes_dir_override"
  elif [ -n "${DASHBOARD_STATE_DIR:-}" ]; then
    printf '%s\n' "$DASHBOARD_STATE_DIR/config/themes"
  elif [ "$(uname -s)" = "Darwin" ]; then
    printf '%s\n' "$HOME/Library/Application Support/my-dashboard/themes"
  else
    printf '%s\n' "${XDG_CONFIG_HOME:-$HOME/.config}/my-dashboard/themes"
  fi
}
themes_dir="$(resolve_themes_dir)"

for dep in whiskers jq; do
  if ! command -v "$dep" >/dev/null 2>&1; then
    echo "error: $dep is not on PATH" >&2
    [ "$dep" = whiskers ] && \
      echo "  install it: cargo install catppuccin-whiskers" >&2 && \
      echo "  or run via nix: nix shell nixpkgs#catppuccin-whiskers -c \$0 $*" >&2
    exit 1
  fi
done
[ -f "$template" ] || { echo "error: template missing: $template" >&2; exit 1; }

# Render in a scratch dir: the template's frontmatter declares the
# flavor matrix, so one invocation writes themes/catppuccin-*.json
# relative to the working directory.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
(cd "$work" && whiskers "$template")

# Install gates mirror flake.nix's: nothing half-valid ships — the run
# fails instead of installing a theme the app would silently degrade.
installed=()
for f in "${flavors[@]}"; do
  src="$work/themes/catppuccin-$f.json"
  if [ ! -f "$src" ]; then
    echo "error: whiskers did not produce catppuccin-$f.json" >&2
    exit 1
  fi
  jq -e '.themes[0].name | startswith("Catppuccin ")' "$src" > /dev/null
  jq -e '.themes[0].mode == "dark" or .themes[0].mode == "light"' "$src" > /dev/null
  jq -e '.themes[0].colors | length == 100' "$src" > /dev/null
done

mkdir -p "$themes_dir"
for f in "${flavors[@]}"; do
  cp "$work/themes/catppuccin-$f.json" "$themes_dir/"
  installed+=("$(jq -r '.themes[0].name' "$themes_dir/catppuccin-$f.json")")
done

echo "installed ${#installed[@]} theme(s) into $themes_dir:"
printf '  - %s\n' "${installed[@]}"
echo "the running app hot-reloads this dir — no restart needed (docs/0025 D1/D5)."
