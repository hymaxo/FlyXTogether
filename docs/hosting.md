# Hosting and joining a session

A FlyXTogether session connects two X-Plane computers directly. One pilot
**hosts**: their simulator flies the aircraft, and they must make one UDP port
reachable from the internet. The other pilot **joins** by typing the host's
address and the session password.

Both pilots need the same aircraft loaded, in the same version. Any aircraft
can be used: FlyXTogether works out what to sync from the aircraft's own
cockpit files. The default Cessna 172 SP (standard, G1000 and seaplane) is
verified; other aircraft work but are untested, so some of their systems may
not sync. Variants of an aircraft count as different aircraft.

## Hosting

1. Load your aircraft.
2. Open **Plugins > FlyXTogether > Open**.
3. On the **Host** tab, keep port `49700` (or pick another UDP port), enter a
   session password, and press **Host**.
4. If Windows asks whether X-Plane may communicate on networks, allow it on
   private networks. Without that, nobody can join.
5. Forward the port on your router (see below), then send your crew:
   - your **public** IP address (search "what is my IP" in a browser),
   - the port, if you changed it from 49700,
   - the session password.

The window then shows `Hosting - waiting for crew`. When your crew joins, it
shows `Connected` with their name.

### Forwarding the port on your router

Your router has to pass incoming traffic on the chosen UDP port to your
computer:

1. Find your computer's address on the home network. The FlyXTogether window
   lists it under "Addresses of this computer" while hosting, usually starting
   with `192.168.` or `10.`.
2. Open your router's admin page (often `http://192.168.1.1` or
   `http://192.168.0.1`) and look for "Port forwarding", "Virtual server" or
   "NAT".
3. Add a rule: protocol **UDP**, external port `49700`, internal port `49700`,
   internal address = your computer's address from step 1.

Router menus differ; your router's manual or the router maker's website shows
the exact steps.

### When port forwarding is not possible

Some internet providers put customers behind "carrier-grade NAT", where the
address your router gets is not your public address. Port forwarding cannot
work then. You can tell by comparing the WAN address on your router's status
page with the result of "what is my IP": if they differ, you are behind
carrier-grade NAT.

Options:

- Let the other pilot host instead.
- Use IPv6 if both of you have it: the host's IPv6 address is listed in the
  window, and only the firewall has to allow the port.
- Use a private network tool such as Tailscale or ZeroTier. The joiner then
  uses the host's address on that network (Tailscale addresses start with
  `100.`).

## Joining

1. Load the same aircraft (and variant) as the host.
2. Open **Plugins > FlyXTogether > Open** and switch to the **Join** tab.
3. Enter the host's address and the session password, then press **Join**.
   The address can be:
   - an IPv4 address, e.g. `203.0.113.7`,
   - an address with a port, e.g. `203.0.113.7:50000`,
   - a host name, e.g. `pilot.example.com`,
   - an IPv6 address, e.g. `[2001:db8::7]:49700`.

   Without a port, `49700` is used.

When the join succeeds, the window shows `Connected`, and the host's aircraft
takes over yours: you ride along while the host flies. Press
**Leave session** to fly on your own again.

## Messages

The texts below are exactly what the window shows. Values such as the port,
address, names and versions are examples.

### While filling in the form

| Message | What to do |
|---|---|
| Enter a port number between 1 and 65535. | Enter a valid UDP port. |
| Enter a session password to host. | A password is required, so strangers cannot join. |
| Enter the host's address, for example 203.0.113.7:49700. | Type the address the host gave you. |
| Enter the session password. | Ask the host for the password. |

### When hosting

| Message | What it means |
|---|---|
| Port 49700 is already in use. Close the program using it or choose another port. | Another program uses that UDP port. Pick another port, and forward that one instead. |
| Load an aircraft before hosting. | No aircraft is loaded yet. Load one, then press Host. |
| Someone tried to join with a wrong password. | A join attempt used the wrong password. You keep waiting for crew. |
| A crew member tried to join with FlyXTogether 0.1.0 (protocol 1), but you have 0.2.0 (protocol 2). The versions are incompatible. | Both of you need compatible FlyXTogether versions; update to the same release. |
| A crew member tried to join with the Cessna 172 SP Seaplane, but you are flying the Cessna 172 SP. | Your crew must load the same aircraft variant as you. |
| Someone tried to join, but the session is full. | A session has two seats. Your current crew is not affected. |
| Alex left the session. | Your crew left. You keep hosting and can accept a new join. |
| Lost connection to Alex. | Nothing was heard from your crew for 10 seconds. You keep hosting. |
| Alex changed aircraft and left the session. | Your crew loaded a different aircraft. |
| Alex's FlyXTogether stopped. | Your crew's plugin was disabled or X-Plane closed. |

### When joining

| Message | What it means |
|---|---|
| Load an aircraft before joining. | No aircraft is loaded yet. Load the host's aircraft, then press Join. |
| Could not reach the host at 203.0.113.7:49700. Check the address, and ask the host to check that their UDP port is forwarded to their computer. | Nothing answered within 10 seconds. Check the address; the host should check port forwarding and the firewall prompt. |
| Wrong password | The password does not match the host's. |
| Too many attempts. Wait a few seconds and try again. | After a wrong password, the host accepts a new attempt after 2 seconds. |
| The FlyXTogether versions are incompatible: the host has 0.1.0 (protocol 1), you have 0.2.0 (protocol 2). | Both of you need compatible FlyXTogether versions. |
| The host is flying the Cessna 172 SP. Load the same aircraft and join again. | Your aircraft variant differs from the host's. |
| The host does not accept your aircraft. | The host runs a different FlyXTogether release. Both of you should use the same release. |
| Session full | The host already has crew. |
| The host is not accepting crew right now. | The host is stopping. Try again later. |
| The host could not prove it knows the session password. You may not be talking to the real host. | The computer at that address is not the host you expected, or someone is intercepting the connection. Check the address. |
| Joining 203.0.113.7:49700 failed: the host did not finish the handshake | Another problem, described after the colon. |

### When a session ends

| Message | What it means |
|---|---|
| The host ended the session. | The host stopped hosting or left. |
| Lost connection to the host. | Nothing was heard from the host for 10 seconds. Your aircraft keeps flying on its own. |
| Session ended: the host changed aircraft. | The host loaded a different aircraft or reloaded it. |
| Session ended: the host's FlyXTogether stopped. | The host's plugin was disabled or X-Plane closed. |
| Session ended because you changed aircraft. | You loaded a different aircraft or reloaded yours. |
