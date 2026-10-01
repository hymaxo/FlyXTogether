# Aircraft sync definitions and profiles

FlyXTogether can sync any X-Plane 12 aircraft without configuration. When
an aircraft loads, the plugin works out what to sync from the aircraft's own
files and from a built-in list. This is called the aircraft's **sync
definition**. A small optional **profile** file can correct the definition
for one aircraft and mark it as verified.

## How the definition is built

1. **The aircraft's cockpit.** The `.acf` file lists the 3D objects the
   aircraft uses. Every clickable thing in those objects is a *manipulator*,
   and each manipulator names either an X-Plane **command** (most buttons,
   switches and knobs) or a **dataref** it moves directly (most levers and
   some knobs). All of them are collected.
2. **The built-in list.** Standard X-Plane cockpit values are added for
   every aircraft: light and electrical switches, fuel pumps and selector,
   mixture, flaps, trim, parking brake, radio frequencies, transponder,
   autopilot settings, altimeter setting and more. The flight-control inputs
   and the systems state (fuel and payload) are added too. The
   list lives in [`crates/flyx-sync/src/definition/builtin.toml`](../crates/flyx-sync/src/definition/builtin.toml).
3. **A profile**, if one in the plugin's `profiles/` folder matches the
   aircraft. It is applied last.

## The classes

| Class | What happens |
|---|---|
| `shared` | A cockpit value. When either pilot changes it, the other seat gets the change. When both change it at once, the host decides the order, so both end with the same value. Every few seconds, values that drifted apart are repaired. |
| `command` | A cockpit command. When either pilot triggers it, by clicking, by keyboard or by joystick, it also runs on the other seat, including how long it is held. Holding the key on START is one example. |
| `state` | A simulated value owned by the pilot flying that would slowly drift apart between the seats, such as fuel. It is copied to the other seat twice a second. |
| `local` | Never synced: views, popups, sound volumes and so on. |

The pilot flying's yoke, pedals, toe brakes and throttles (`input`) are
handled by the built-in list, together with the overrides the other seat
uses while following (`monitor_override`). Profiles rarely need to touch
them.

Instruments need no entries at all. The following seat's flight model
keeps running, with its aircraft put where the pilot flying's is every
frame, so its engines, gyros, air-data instruments, electrics and avionics
simulate themselves from the same state and the same cockpit.

A command and its result usually both sync. For example, the
landing-light switch command runs on both seats, and the
`landing_lights_on` value syncs too. That is intended: when a seat
runs a command because the other pilot pressed it, the changes it
causes are recognised as coming from the other seat and are not sent
back.

## When to write a profile

Most aircraft work without one. Write a profile when:

- something does not sync, because the aircraft drives it from its own
  plugin without a cockpit command (add a `shared` dataref or a `command`);
- something should not sync, such as a seat-specific gadget (make it `local`);
- the following seat shows wrong values for a simulated quantity (add a
  `state` dataref);
- you have flown the aircraft together and everything works (set
  `verified = true`, which removes the "untested aircraft" line in the
  window).

**To find out what an aircraft uses:**
- `FlyXTogether.log` lists the definition for each aircraft you load: how many entries it has, and which datarefs or commands X-Plane did not know.
- DataRefTool and similar plugins show datarefs while you click.

## Format

Profiles are TOML files in `Resources/plugins/FlyXTogether/profiles/`.

**Both pilots need identical definitions.** If one pilot edits a profile,
the other pilot needs the same file, or joining is refused with "The
aircraft files or profiles differ between the two seats."

A complete example:

```toml
format = 1
name = "Example Twin"
verified = false

# Which aircraft this profile is for: the folder that holds the .acf and
# the .acf file name. Each one is a variant with its own id.
[[variant]]
id = "standard"
folder = "Example Twin"
acf = "example_twin.acf"

[[variant]]
id = "glass"
folder = "Example Twin"
acf = "example_twin_glass.acf"

# Add a value the generator cannot see. Array datarefs need an index:
# a number, a list, or "per_engine".
[[shared]]
dataref = "example/systems/hydraulic_pump_switch"
index = "per_engine"

# Only for one variant.
[[command]]
command = "example/mfd/softkey_{1..12}"
variants = ["glass"]

# Fuel in a custom tank, owned by the pilot flying.
[[state]]
dataref = "example/fuel/aux_tank_kg"
epsilon = 0.1

# Never sync these. `*` matches any text.
[[local]]
command = "example/cabin/*"

[[local]]
dataref = "example/sound/*"
```

**Entry keys:**
- `dataref`, `command`: the name. Braces expand: `a_{1,2}` means `a_1` and `a_2`, and `x{1..3}` means `x1`, `x2` and `x3`. Only `local` names may contain `*`.
- `index`: for array datarefs. A number, a list (`[0, 1]`) or `"per_engine"` (one element per engine of the aircraft).
- `epsilon`: the smallest change of a fractional value that counts as a change (default 0.0001).
- `variants`: limits the entry to some of the profile's variants.

**Format version.** `format = 1` is the only format so far. A file with a
syntax error is skipped as a whole, and the log names the file and line.
An entry naming something X-Plane does not know is skipped with a warning.
