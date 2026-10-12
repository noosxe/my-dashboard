#!/bin/sh
# In-container session bootstrap (docs/0071 D5): every `docker run`
# lands inside a dbus session with the headless-sway environment the
# debug bridge needs — builds, tests and receipt verbs all run without
# per-run setup. Signals propagate: `exec` + `dbus-run-session --` make
# the wrapped command's exit code the container's.
set -eu

mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"

export WLR_BACKENDS=headless \
       WLR_LIBINPUT_NO_DEVICES=1 \
       WLR_RENDERER=pixman

exec dbus-run-session -- "$@"
