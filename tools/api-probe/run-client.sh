#!/bin/bash
# Run one client in the private session and record what it uses.
# usage: run-client.sh NAME SECONDS [--hook 'snippet run after HOOK_DELAY s'] -- COMMAND...
# Writes $API_PROBE_DIR/logs/NAME.{wl.log,stdout,session.dbus,system.dbus,*.names,summary.json,dbus-summary.json}
set -u
here=$(cd "$(dirname "$0")" && pwd); . "$here/env.sh" || exit 1
probe_private_dir || exit 1
name=$1; secs=$2; shift 2; hook=
case "$name" in *[!A-Za-z0-9_.-]*|'') echo "NAME may contain only letters, digits, _ . -" >&2; exit 1 ;; esac
if [ "${1:-}" = --hook ]; then hook=$2; shift 2; fi
[ "${1:-}" = -- ] && shift
export WAYLAND_DISPLAY=${TARGET_DISPLAY:-$PROBE_DISPLAY}
log=$API_PROBE_DIR/logs/$name
dbus-monitor --address "$DBUS_SESSION_BUS_ADDRESS" >"$log.session.dbus" 2>/dev/null & m1=$!
dbus-monitor --address "$DBUS_SYSTEM_BUS_ADDRESS" >"$log.system.dbus" 2>/dev/null & m2=$!
sleep 0.3
( $PROBE_REAL_ENV systemd-run --user --scope --quiet --collect --unit="$PROBE_UNIT_PREFIX-c-$name" -p MemoryMax="${MEM:-4G}" -p MemorySwapMax=0 -- \
    env WAYLAND_DEBUG=1 XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" DBUS_SESSION_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS" "$@" >"$log.stdout" 2>"$log.wl.log" ) &
client=$!
if [ -n "$hook" ]; then sleep "${HOOK_DELAY:-4}"; ( unset WAYLAND_DEBUG; eval "$hook" ) >"$log.hook.log" 2>&1; fi
SECONDS=0; while [ "$SECONDS" -lt "$secs" ] && kill -0 "$client" 2>/dev/null; do sleep 0.5; done
kill "$m1" "$m2" 2>/dev/null
busctl --address="$DBUS_SESSION_BUS_ADDRESS" list --no-pager >"$log.session.names" 2>&1
busctl --address="$DBUS_SYSTEM_BUS_ADDRESS" list --no-pager >"$log.system.names" 2>&1
$PROBE_REAL_ENV systemctl --user kill --signal=TERM "$PROBE_UNIT_PREFIX-c-$name.scope" 2>/dev/null; sleep 1.5
$PROBE_REAL_ENV systemctl --user kill --signal=KILL "$PROBE_UNIT_PREFIX-c-$name.scope" 2>/dev/null
wait "$client" 2>/dev/null; echo "exit=$?" >>"$log.stdout"
python3 -I "$here/wl-globals.py" "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" >"$API_PROBE_DIR/logs/advertised.tsv"
python3 -I "$here/wl-summary.py" "$log.wl.log" "$API_PROBE_DIR/logs/advertised.tsv" >"$log.summary.json" && python3 -I "$here/wl-summary.py" --print "$log.summary.json"
python3 -I "$here/dbus-summary.py" "$log.session.dbus" "$log.system.dbus" >"$log.dbus-summary.json"
