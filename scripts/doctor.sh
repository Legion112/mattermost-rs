#!/usr/bin/env bash
# Check that the build environment is the one we think it is.
#
# This exists because of one specific trap: Homebrew installs 403 .pc files
# into /home/linuxbrew/.linuxbrew/lib/pkgconfig, they precede the system ones,
# and they describe a GTK (4.22) that is not the GTK this machine runs (apt's
# 4.14). Building against them produces a binary linked to
# /home/linuxbrew/.linuxbrew/lib/libgtk-4.so that matches neither CI nor the
# .deb, and the symptom is a confusing runtime crash rather than a build error.
set -uo pipefail

export PKG_CONFIG_LIBDIR=/usr/lib/x86_64-linux-gnu/pkgconfig:/usr/share/pkgconfig
export PKG_CONFIG_PATH=
WANT_GTK=4.14
WANT_ADW=1.5
fail=0

say() { printf '  %-22s %s\n' "$1" "$2"; }

echo "toolchain"
say "rustc" "$(rustc --version 2>&1)"
say "cargo" "$(cargo --version 2>&1)"
if cargo llvm-cov --version >/dev/null 2>&1; then
  say "cargo-llvm-cov" "$(cargo llvm-cov --version 2>&1)"
else
  say "cargo-llvm-cov" "MISSING — cargo install cargo-llvm-cov (make cover-gate needs it)"
  fail=1
fi

echo "system libraries (pkg-config, Homebrew excluded)"
check_mod() {
  local mod="$1" want="$2" got
  if ! got=$(pkg-config --modversion "$mod" 2>/dev/null); then
    say "$mod" "NOT FOUND — run 'make apt-deps'"
    fail=1
    return
  fi
  case "$got" in
    "$want".*) say "$mod" "$got" ;;
    *) say "$mod" "$got — expected $want.x; something other than the apt package is on PKG_CONFIG_LIBDIR"
       fail=1 ;;
  esac
}
check_mod gtk4 "$WANT_GTK"
check_mod libadwaita-1 "$WANT_ADW"

# Report the shadowing explicitly, so that a future reader does not have to
# rediscover why PKG_CONFIG_LIBDIR is pinned.
if [ -d /home/linuxbrew/.linuxbrew/lib/pkgconfig ]; then
  n=$(find /home/linuxbrew/.linuxbrew/lib/pkgconfig -name '*.pc' 2>/dev/null | wc -l)
  # Probe Homebrew's own .pc files explicitly. Clearing both variables would
  # leave pkg-config with no search path at all and report "none" regardless.
  brew_gtk=$(env -u PKG_CONFIG_LIBDIR \
    PKG_CONFIG_PATH=/home/linuxbrew/.linuxbrew/lib/pkgconfig \
    pkg-config --modversion gtk4 2>/dev/null || echo "not installed there")
  say "homebrew .pc files" "$n present (its gtk4 is $brew_gtk) — excluded, as intended"
fi

echo "lab prerequisites"
if docker info >/dev/null 2>&1; then
  say "docker" "$(docker --version 2>&1)"
else
  say "docker" "daemon not answering — 'make lab' will not run"
fi
rustup target list --installed 2>/dev/null | grep -q musl \
  && say "musl target" "installed" \
  || { say "musl target" "MISSING — rustup target add x86_64-unknown-linux-musl"; fail=1; }

echo
if [ "$fail" -eq 0 ]; then
  echo "doctor: environment looks right"
else
  echo "doctor: problems above need fixing"
fi
exit "$fail"
