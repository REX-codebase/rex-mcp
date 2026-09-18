#!/usr/bin/env bash
# Compile the Linux Tauri shell without root by pointing pkg-config and the
# linker at an extracted Debian/Ubuntu sysroot.
set -euo pipefail
ROOT="${REX_TAURI_SYSROOT:-$HOME/.cache/rex-harness/tauri-sysroot}"
if [[ ! -f "$ROOT/usr/lib/x86_64-linux-gnu/pkgconfig/webkit2gtk-4.1.pc" ]]; then
  echo "missing WebKitGTK sysroot at $ROOT" >&2
  echo "Use a normal Tauri build host, or set REX_TAURI_SYSROOT to an extracted sysroot." >&2
  exit 2
fi
export PKG_CONFIG_SYSROOT_DIR="$ROOT"
export PKG_CONFIG_PATH="$ROOT/usr/lib/x86_64-linux-gnu/pkgconfig:$ROOT/usr/share/pkgconfig"
export PKG_CONFIG_ALLOW_SYSTEM_CFLAGS=1
export LIBRARY_PATH="$ROOT/usr/lib/x86_64-linux-gnu${LIBRARY_PATH:+:$LIBRARY_PATH}"
export LD_LIBRARY_PATH="$ROOT/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,-rpath-link,$ROOT/usr/lib/x86_64-linux-gnu -L native=$ROOT/usr/lib/x86_64-linux-gnu"
exec "$@"
