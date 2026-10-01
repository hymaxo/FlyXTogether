#!/usr/bin/env bash
# Builds the plugin and copies it into a local X-Plane 12 for testing.
# Usage: XPLANE_ROOT="/path/to/X-Plane 12" scripts/dev-install.sh [--dev] [--debug]
#   --dev    include developer menu items (feature "dev")
#   --debug  debug build instead of release
set -euo pipefail

: "${XPLANE_ROOT:?set XPLANE_ROOT to your X-Plane 12 folder}"
root="$(cd "$(dirname "$0")/.." && pwd)"
profile=release
features=()
for arg in "$@"; do
  case "$arg" in
    --dev) features=(--features dev) ;;
    --debug) profile=debug ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

cargo_args=(build -p flyx-plugin "${features[@]}")
[ "$profile" = release ] && cargo_args+=(--release)
cargo "${cargo_args[@]}" --manifest-path "$root/Cargo.toml"

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) platform=win_x64; lib=FlyXTogether.dll ;;
  Darwin) platform=mac_x64; lib=libFlyXTogether.dylib ;;
  Linux) platform=lin_x64; lib=libFlyXTogether.so ;;
  *) echo "unsupported platform" >&2; exit 1 ;;
esac

dest="$XPLANE_ROOT/Resources/plugins/FlyXTogether/$platform"
mkdir -p "$dest"

# Copies $1 to $2. If X-Plane is running with the plugin loaded, Windows
# locks the file but still allows renaming it, so the old copy is moved
# aside and the new one is picked up at the next plugin reload.
install_file() {
  rm -f "$2".old-* 2>/dev/null || true
  if ! cp "$1" "$2" 2>/dev/null; then
    mv "$2" "$2.old-$(date +%s)"
    cp "$1" "$2"
    echo "note: $(basename "$2") was in use; the new build loads on the next plugin reload"
  fi
}

install_file "$root/target/$profile/$lib" "$dest/FlyXTogether.xpl"
# Debug symbols make panic backtraces in FlyXTogether.log readable.
if [ "$platform" = win_x64 ] && [ -f "$root/target/$profile/FlyXTogether.pdb" ]; then
  install_file "$root/target/$profile/FlyXTogether.pdb" "$dest/FlyXTogether.pdb"
fi
# Aircraft profiles ship next to the platform folders.
mkdir -p "$XPLANE_ROOT/Resources/plugins/FlyXTogether/profiles"
cp "$root"/profiles/*.toml "$XPLANE_ROOT/Resources/plugins/FlyXTogether/profiles/"
echo "installed $profile build to $dest/FlyXTogether.xpl, profiles to FlyXTogether/profiles"
