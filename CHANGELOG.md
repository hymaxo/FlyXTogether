# Changelog

## 0.2.0-alpha.1

Two pilots, one cockpit, and now either of you can fly. 🧑‍✈️🤝👩‍✈️

> [!WARNING]
> **Still alpha.** Not compatible with 0.1.0-alpha.1: both pilots need this
> version. Please [report bugs](https://github.com/hymaxo/FlyXTogether/issues)
> with your `FlyXTogether.log` attached.

### New

- **Shared cockpit.** Switches, knobs, radios, autopilot settings, fuel and
  the magnetos sync both ways. Buttons and held commands (such as the
  starter) run on both seats. Joining sends the host's cockpit to the crew.
- **Take controls.** Either pilot can become the pilot flying (PF) with the
  **Take controls** button, the Plugins menu item or a key binding. The
  aircraft continues without a jump.
- **Live instruments for the pilot monitoring (PM).** Its flight model keeps
  running with the aircraft placed where the PF's is, so every gauge, the
  engine, the electrics and the avionics work on their own. The PF's yoke,
  throttle and trim move in the PM's cockpit; the PM's own joystick and
  throttle do nothing. A PM engine that is stopped while the PF's runs (or
  the other way round) is started or stopped to match.
- **Any aircraft.** What to sync is worked out from the aircraft's own cockpit
  files. Aircraft without a verified profile are marked untested; the
  Cessna 172 SP (all three variants) is verified. See
  [docs/profiles.md](https://github.com/hymaxo/FlyXTogether/blob/main/docs/profiles.md).
- **Paste** (Ctrl+V, Cmd+V on macOS) in the FlyXTogether window.

### Fixed since the first nightly

- The PM's engine stayed at idle RPM while the PF's throttle was full.
- Elevator trim moved by the autopilot drifted apart between the seats.
- The C172 fuel cutoff pulled on one seat did nothing on the other.
- A PM joystick or throttle could fight the PF's inputs.
- Battery charge drifted apart between the seats.

### Known limitations

- Joining in mid-flight works, but aircraft that keep switch positions in
  their own scripts (such as the Citation X) can start out of step. Join on
  the ground.
- On the PM, dragging the C172 trim wheel has no effect (the trim buttons
  work). Trim belongs to the PF.

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
