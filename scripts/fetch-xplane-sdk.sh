#!/usr/bin/env bash
# Downloads the X-Plane SDK and installs the macOS frameworks into
# third_party/xplane-sdk/Libraries/Mac. The frameworks rely on symlinks,
# so they are not committed (see third_party/README.md).
set -euo pipefail

SDK_VERSION="430"
SDK_URL="https://developer.x-plane.com/wp-content/plugins/code-sample-generation/sdk_zip_files/XPSDK${SDK_VERSION}.zip"
SDK_SHA256="b9875ab27b593927b4f9b3e0ddfffe7401ee5dce6d86b50aea0da65f70ff7816"

root="$(cd "$(dirname "$0")/.." && pwd)"
dest="$root/third_party/xplane-sdk/Libraries/Mac"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

curl -fsSL -o "$tmp/sdk.zip" "$SDK_URL"
if command -v sha256sum >/dev/null; then
  actual="$(sha256sum "$tmp/sdk.zip" | cut -d' ' -f1)"
else
  actual="$(shasum -a 256 "$tmp/sdk.zip" | cut -d' ' -f1)"
fi
if [ "$actual" != "$SDK_SHA256" ]; then
  echo "SDK checksum mismatch: expected $SDK_SHA256, got $actual" >&2
  exit 1
fi

unzip -q "$tmp/sdk.zip" -d "$tmp"
rm -rf "$dest"
mkdir -p "$(dirname "$dest")"
cp -R "$tmp/SDK/Libraries/Mac" "$dest"
echo "Installed X-Plane SDK ${SDK_VERSION} macOS frameworks into $dest"
