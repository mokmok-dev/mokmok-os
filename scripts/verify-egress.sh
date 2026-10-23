#!/usr/bin/env bash
# Runs the Kani model checks in verification/egress-destination.
#
# The pinned bundle is Kani 0.68.0, which is built against nightly-2026-08-21. The script downloads
# the bundle once into ${XDG_CACHE_HOME:-$HOME/.cache}/mokmok/kani, verifies its checksum, and
# reuses it afterwards. Arguments are passed through to kani-driver, so
# `--harness <name>` and `--output-format regular` work as usual.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
CRATE="$ROOT/verification/egress-destination"
KANI_VERSION=0.68.0
KANI_SHA256=32e2b484d73ede0bbf64a2cf0879c4259422497d8aec4ee67d448de9ae7843d3
TOOLCHAIN=nightly-2026-08-21-x86_64-unknown-linux-gnu
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/mokmok/kani"
BUNDLE="$CACHE/kani-$KANI_VERSION"
TARBALL="$CACHE/kani-$KANI_VERSION-x86_64-unknown-linux-gnu.tar.gz"

if [[ ! -x "$BUNDLE/bin/kani-driver" ]]; then
  mkdir -p "$CACHE"
  if [[ ! -f "$TARBALL" ]]; then
    echo "downloading Kani $KANI_VERSION" >&2
    curl -sSLf -o "$TARBALL" \
      "https://github.com/model-checking/kani/releases/download/kani-$KANI_VERSION/kani-$KANI_VERSION-x86_64-unknown-linux-gnu.tar.gz"
  fi
  echo "$KANI_SHA256  $TARBALL" | sha256sum -c - >&2
  tar -xzf "$TARBALL" -C "$CACHE"
fi

RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
if [[ -x "$RUSTUP_HOME/toolchains/$TOOLCHAIN/bin/rustc" ]]; then
  TC="$RUSTUP_HOME/toolchains/$TOOLCHAIN"
elif command -v rustup >/dev/null 2>&1 && rustup run "$TOOLCHAIN" rustc --version >/dev/null 2>&1; then
  TC=$(rustup run "$TOOLCHAIN" rustc --print sysroot)
else
  echo "error: $TOOLCHAIN is not installed." >&2
  echo "       rustup toolchain install $TOOLCHAIN" >&2
  exit 2
fi

EXPECTED=$(tr -d '\n' < "$BUNDLE/rustc-version")
ACTUAL=$("$TC/bin/rustc" --version)
if [[ "$ACTUAL" != "$EXPECTED" ]]; then
  echo "error: Kani $KANI_VERSION needs '$EXPECTED', found '$ACTUAL'." >&2
  exit 2
fi

ln -sfn "$TC" "$BUNDLE/toolchain"

# NixOS binaries from the release bundle carry a generic ELF interpreter that does not exist.
if ! command -v patchelf >/dev/null 2>&1 && [[ -e /etc/NIXOS ]]; then
  echo "error: patchelf is required to run the Kani bundle on NixOS." >&2
  echo "       nix shell nixpkgs#patchelf --command bash scripts/verify-egress.sh" >&2
  exit 2
fi
if command -v patchelf >/dev/null 2>&1; then
  if ! patchelf --print-interpreter "$BUNDLE/bin/kani-driver" >/dev/null 2>&1; then
    :
  else
    INTERP=$(patchelf --print-interpreter "$BUNDLE/bin/kani-driver")
    NEEDS_PATCH=0
    if [[ ! -x "$INTERP" ]]; then
      NEEDS_PATCH=1
    fi
    # On NixOS `/lib64/ld-linux-x86-64.so.2` exists as a stub that refuses to run, so the path is
    # executable while still being unusable.
    if [[ "$INTERP" == "/lib64/ld-linux-x86-64.so.2" ]] &&
      [[ -e /etc/NIXOS || -n "${NIX_CC:-}" ]]; then
      NEEDS_PATCH=1
    fi
    if [[ "$NEEDS_PATCH" -eq 1 ]]; then
      LOADER=""
      if [[ -n "${NIX_CC:-}" && -f "$NIX_CC/nix-support/dynamic-linker" ]]; then
        LOADER=$(cat "$NIX_CC/nix-support/dynamic-linker")
      fi
      if [[ -z "$LOADER" || ! -x "$LOADER" ]]; then
        LOADER=$(find /nix/store -maxdepth 4 -name 'ld-linux-x86-64.so.2' 2>/dev/null | head -n 1)
      fi
      if [[ -z "$LOADER" ]]; then
        echo "error: NixOS interpreter patch failed; install patchelf and retry." >&2
        exit 2
      fi
      echo "patching Kani binaries for NixOS (interpreter $LOADER)" >&2
      for binary in "$BUNDLE"/bin/*; do
        if patchelf --print-interpreter "$binary" >/dev/null 2>&1; then
          patchelf --set-interpreter "$LOADER" "$binary"
        fi
      done
    fi
  fi
fi

# The rustup-provided toolchain on this host has an `ld.lld` shim whose wrapper script was
# garbage-collected. Point it at the real linker the bundle ships with.
LLD="$TC/lib/rustlib/x86_64-unknown-linux-gnu/bin/gcc-ld/ld.lld"
REAL_LLD="$TC/lib/rustlib/x86_64-unknown-linux-gnu/bin/gcc-ld-unwrapped/ld.lld"
if [[ -f "$LLD" && -x "$REAL_LLD" ]]; then
  MISSING=""
  for path in $(grep -o '/nix/store/[^"[:space:]]*' "$LLD" 2>/dev/null || true); do
    if [[ ! -e "$path" ]]; then
      MISSING="$path"
      break
    fi
  done
  if [[ -n "$MISSING" ]]; then
    echo "patching the toolchain lld shim (backup ${LLD}.mokmok-backup)" >&2
    cp "$LLD" "$LLD.mokmok-backup"
    printf '#!/usr/bin/env bash\nexec "%s" "$@"\n' "$REAL_LLD" > "$LLD"
    chmod +x "$LLD"
  fi
fi

LD_PATH="$TC/lib"
LIBSTDCXX=$(find /nix/store -maxdepth 4 -name 'libstdc++.so.6' 2>/dev/null | head -n 1)
if [[ -n "$LIBSTDCXX" ]]; then
  LD_PATH="$LD_PATH:$(dirname "$LIBSTDCXX")"
fi

unset CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER
unset CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER

cd "$CRATE"
export PATH="$TC/bin:$BUNDLE/bin:$PATH"
export RUSTUP_TOOLCHAIN="$TOOLCHAIN"
export LD_LIBRARY_PATH="$LD_PATH${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

ARGS=("$@")
if [[ " $* " != *" --output-format "* ]]; then
  ARGS+=(--output-format terse)
fi

exec -a cargo-kani "$BUNDLE/bin/kani-driver" \
  --manifest-path "$CRATE/Cargo.toml" "${ARGS[@]}"
