#!/usr/bin/env bash
# Exercises session handling against X-Plane on this computer.
#
#   scripts/session-check.sh join [password] [port]
#       X-Plane must be hosting. Runs: a correct join that leaves after a
#       few seconds, a wrong password, and a third seat while one is joined.
#   scripts/session-check.sh host [password] [port]
#       Hosts with a synthetic flight for 5 minutes; join it from X-Plane.
#
# The flyx-peer seats claim the standard Cessna 172 SP (Cessna_172SP.acf).
set -u
mode="${1:-join}"
password="${2:-test}"
port="${3:-49700}"
root="$(cd "$(dirname "$0")/.." && pwd)"
peer="$root/target/release/flyx-peer"
cargo build -q -p flyx-peer --release --manifest-path "$root/Cargo.toml" || exit 1

seat() { # name password seconds
  "$peer" --name "$1" --password "$2" --leave-after "$3" join "127.0.0.1:$port" 2>&1 |
    sed -u "s/^/[$1] /"
}

case "$mode" in
  join)
    echo "== 1. Bot joins with the right password and leaves after 8 s"
    seat Bot "$password" 8
    sleep 3
    echo "== 2. Intruder tries a wrong password"
    seat Intruder not-the-password 15
    sleep 3
    echo "== 3. Bot joins; Third tries to join while Bot is connected"
    seat Bot "$password" 12 &
    sleep 5
    seat Third "$password" 15
    wait
    echo "== done"
    ;;
  host)
    echo "== hosting on port $port with password '$password' for 5 minutes;"
    echo "   in X-Plane, join 127.0.0.1:$port"
    "$peer" --name Bot --password "$password" --leave-after 300 host --port "$port" --flight parked
    ;;
  *)
    echo "usage: $0 join|host [password] [port]" >&2
    exit 2
    ;;
esac
