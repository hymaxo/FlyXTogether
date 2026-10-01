# Changelog

## Unreleased

Available as the [nightly build](https://github.com/hymaxo/FlyXTogether/releases/tag/nightly).
Not compatible with 0.1.0-alpha.1: both pilots need the same build.

- **Shared cockpit.** Switches, knobs, radios, autopilot settings, fuel and
  the magnetos sync both ways. Buttons and held commands (such as the starter)
  run on both seats.
- **Take controls.** Either pilot can take the flight controls with the
  **Take controls** button, the Plugins menu item or a key binding. The
  aircraft continues without a jump.
- **Live instruments on the following seat.** Its flight model keeps running
  with the aircraft placed where the pilot flying's is, so every gauge, the
  engine and the avionics work on their own. A stopped engine is started to
  match the pilot flying's.
- **Any aircraft.** What to sync is worked out from the aircraft's own cockpit
  files. Aircraft without a verified profile are marked untested; the
  Cessna 172 SP (all three variants) is verified. See
  [docs/profiles.md](https://github.com/hymaxo/FlyXTogether/blob/main/docs/profiles.md).

## 0.1.0-alpha.1

The very first release. Hello, world! 👋✈️

> [!WARNING]
> **Alpha software.** This is an early preview for curious pilots. It works in
> our testing on Windows, but expect bugs, and please
> [report them](https://github.com/hymaxo/FlyXTogether/issues) with your
> `FlyXTogether.log` attached.

### What you get

- **Cessna 172 SP support**: the default C172 SP, G1000 and Seaplane variants.
  Both pilots must load the same variant.
- **Host flies, crew rides along.** The joining pilot's aircraft follows the
  host's position, attitude, control surfaces, flaps, nosewheel steering and
  propeller, smoothed by an adaptive playout buffer.
- **Direct connection** over UDP port 49700 (configurable). The host forwards
  the port on their router. See the [hosting guide](https://github.com/hymaxo/FlyXTogether/blob/main/docs/hosting.md).
- **Password-protected, encrypted sessions** (QUIC + TLS 1.3 with a mutual
  password proof). No accounts, no third-party servers.
- **Safe hand-back:** the aircraft is always returned to X-Plane when a session
  ends: host leaves, connection lost, plugin disabled, or an internal error.

### Known limitations

- The crew cannot take the controls yet, and cockpit switches and instruments
  are not synced.
- No relay: if the host cannot forward a port (carrier-grade NAT), use IPv6,
  Tailscale/ZeroTier, or let the other pilot host.
- Windows is tested in X-Plane. macOS (universal) and Linux builds are included
  but have not been flown yet.
- VR is untested.
- On macOS, remove the quarantine flag after unzipping (see the README).

### Install

Unzip `FlyXTogether-0.1.0-alpha.1.zip` into `X-Plane 12/Resources/plugins/`,
start X-Plane 12.4 or newer, and open **Plugins → FlyXTogether → Open**.
