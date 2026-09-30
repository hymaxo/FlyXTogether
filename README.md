<div align="center">

# ✈️ FlyXTogether

**Fly one airplane with a friend in X-Plane 12: free and open source.**

A shared-cockpit (multicrew) plugin, in the spirit of SmartCopilot and SharedFlight.
Two pilots, two simulators, one aircraft, connected directly over the internet.
No accounts, no servers, no subscriptions.

[![build](https://github.com/hymaxo/FlyXTogether/actions/workflows/build.yml/badge.svg?branch=main)](https://github.com/hymaxo/FlyXTogether/actions/workflows/build.yml)
[![release](https://img.shields.io/github/v/release/hymaxo/FlyXTogether?include_prereleases&sort=semver&label=release)](https://github.com/hymaxo/FlyXTogether/releases)
[![X-Plane 12](https://img.shields.io/badge/X--Plane-12.4%2B-1f6feb)](https://www.x-plane.com/)
[![license](https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-green)](#-license)

</div>

```
   🧑‍✈️ Captain (host)                          👩‍✈️ Crew (joins)
  ┌──────────────────┐     your internet     ┌──────────────────┐
  │  X-Plane 12      │ ───────────────────▶  │  X-Plane 12      │
  │  flies the plane │   encrypted, direct   │  rides along     │
  └──────────────────┘                       └──────────────────┘
```

> [!WARNING]
> **This is an alpha.** It works in our testing, but it's early.
> Expect rough edges, and please don't rely on it for your 12-hour
> online event yet. Found a bug? [Open an issue](https://github.com/hymaxo/FlyXTogether/issues).
> It helps a lot! 💛

## 🛩️ What works today

- **Cessna 172 SP**: the default one, including the **G1000** and **Seaplane** variants.
- **Host flies, crew rides along.** The crew's aircraft follows the host's exactly:
  position, attitude, control surfaces, flaps, nosewheel steering and propeller.
- **Smooth over real internet connections.** A playout buffer hides jitter and
  lost packets, so the ride looks fluid, not teleporty.
- **Direct and private.** Password-protected sessions, encrypted end to end
  (QUIC + TLS). Nothing goes through anyone else's server.
- **Plays nice with X-Plane.** Crashes inside the plugin are caught, and the
  aircraft is always handed back to you when a session ends.

## 🚧 Not yet (coming later)

- Crew taking the controls / handing over control
- Cockpit sync (switches, radios, instruments)
- SmartCopilot profile import, and more aircraft (ToLiss A321 is next)
- Joining without port forwarding (relay or NAT traversal)
- Tested on macOS, Linux and VR. The builds are there, but nobody has flown
  them yet. Reports welcome!

## 📦 Install

1. Grab `FlyXTogether-<version>.zip` from the
   [**Releases**](https://github.com/hymaxo/FlyXTogether/releases) page.
2. Unzip it into `X-Plane 12/Resources/plugins/`. You should end up with
   `Resources/plugins/FlyXTogether/win_x64/FlyXTogether.xpl`
   (plus `mac_x64/` and `lin_x64/`).
3. Start X-Plane. You'll find it under **Plugins → FlyXTogether**.

Both pilots need the **same FlyXTogether version** and the **same aircraft**.

<details>
<summary>🍎 macOS: allow the unsigned plugin</summary>

The plugin isn't notarized yet, so macOS quarantines it after download. Run this
once after unzipping:

```bash
xattr -dr com.apple.quarantine "/path/to/X-Plane 12/Resources/plugins/FlyXTogether"
```

</details>

<details>
<summary>🌙 Nightly builds</summary>

Every commit to `main` is built automatically and published as the
[**nightly**](https://github.com/hymaxo/FlyXTogether/releases/tag/nightly)
pre-release. It has the newest stuff, and the newest bugs. Both pilots must use
the same build.

</details>

## 🎮 Fly together in 60 seconds

**Captain (host):**

1. Load the Cessna 172 SP.
2. **Plugins → FlyXTogether → Open**, **Host** tab, pick a password, press **Host**.
3. Forward UDP port **49700** on your router to your computer.
4. Send your crew your public IP address and the password.

**Crew (join):**

1. Load the same Cessna 172 SP variant.
2. **Plugins → FlyXTogether → Open**, **Join** tab, enter the address and password, press **Join**.
3. Sit back and enjoy the view. **Leave session** gives you your own plane back.

Port forwarding, carrier-grade NAT, Tailscale/ZeroTier, and what every message
in the window means are covered in the **[hosting guide](docs/hosting.md)**.

## 🐞 Reporting a problem

The plugin keeps a log next to itself:
`Resources/plugins/FlyXTogether/FlyXTogether.log`. After a crash, the log from
the run before is `FlyXTogether.previous.log`. Please attach it (and X-Plane's
`Log.txt`) to your issue. Passwords are never logged.

## 🛠️ Building from source

You need [rustup](https://rustup.rs/) and a C++ compiler (Dear ImGui is built
from source). The Rust version is pinned in `rust-toolchain.toml` and installed
automatically.

- **Windows:** Visual Studio Build Tools with the C++ workload
- **macOS:** `xcode-select --install`, then run `scripts/fetch-xplane-sdk.sh` once
- **Linux:** `sudo apt install build-essential libgl-dev`

```bash
git clone https://github.com/hymaxo/FlyXTogether.git
cd FlyXTogether
cargo test --workspace
cargo build -p flyx-plugin --release
```

Build and copy straight into your X-Plane (Git Bash on Windows, any shell
elsewhere). `--dev` adds developer menu items:

```bash
XPLANE_ROOT="/path/to/X-Plane 12" scripts/dev-install.sh
```

<details>
<summary>More developer notes</summary>

The plugin library lands in `target/release/`:

| Platform | Built file | Install as |
|---|---|---|
| Windows | `FlyXTogether.dll` | `FlyXTogether/win_x64/FlyXTogether.xpl` |
| macOS | `libFlyXTogether.dylib` | `FlyXTogether/mac_x64/FlyXTogether.xpl` |
| Linux | `libFlyXTogether.so` | `FlyXTogether/lin_x64/FlyXTogether.xpl` |

`tools/flyx-peer` is a headless peer that can host or join without X-Plane. It's
handy for testing the plugin alone, e.g. `cargo run -p flyx-peer -- host --flight circuit`.

`crates/flyx-xplm/src/sys.rs` is generated from the X-Plane SDK headers with
`scripts/gen-bindings.sh` (needs `bindgen-cli` and libclang; on Windows put
libclang's folder on `PATH` and in `LIBCLANG_PATH`).

| Path | Contents |
|---|---|
| `crates/flyx-protocol` | network message types |
| `crates/flyx-sync` | session state machine and flight-state playout (no X-Plane needed) |
| `crates/flyx-net` | QUIC transport and password handshake |
| `crates/flyx-xplm` | safe Rust wrappers over the X-Plane SDK |
| `crates/flyx-plugin` | the plugin itself |
| `tools/flyx-peer` | headless test peer |

**Releases:** bump `version` in `Cargo.toml`, add a section to
`CHANGELOG.md`, and push a tag `v<version>`. GitHub Actions builds all three
platforms and publishes the release.

</details>

## 📜 License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option. The X-Plane SDK in `third_party/xplane-sdk/` keeps its own license.

<div align="center">

Made with ☕ and way too many touch-and-goes. Blue skies! 🌤️

</div>
