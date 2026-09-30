# Third-party files

## X-Plane SDK 4.3.0 (`xplane-sdk/`)

- Source: https://developer.x-plane.com/sdk/plugin-sdk-downloads/ (`XPSDK430.zip`,
  SHA-256 `b9875ab27b593927b4f9b3e0ddfffe7401ee5dce6d86b50aea0da65f70ff7816`)
- License: BSD-style, see `xplane-sdk/license.txt`. Redistribution is permitted as
  long as the copyright notice is kept, so the files are committed here.
- Requires X-Plane 12.4.0 or newer.

What is committed:

| Path | Used for |
|---|---|
| `CHeaders/` | generating the Rust bindings in `crates/flyx-xplm` |
| `Libraries/Win/XPLM_64.lib` | linking the Windows plugin |

What is **not** committed:

- `Libraries/Mac/*.framework` use symlinks, which do not survive a checkout on
  Windows. Run `scripts/fetch-xplane-sdk.sh` on macOS (CI does this) to install
  them from the same zip, verified by checksum.
- Linux needs no link-time library. XPLM symbols stay undefined and X-Plane
  resolves them when it loads the plugin.
- `Delphi/` and the widget/wrapper libraries are not used.

To point the build at a different SDK copy, set `FLYX_XPLANE_SDK` to its root
(the folder containing `CHeaders/`).
