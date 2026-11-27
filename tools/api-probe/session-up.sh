#!/bin/bash
# Start the private session: session bus, stand-in system bus, a headless Weston as host and the
# compositor under test nested inside it. Nothing touches the live desktop.
# usage: API_PROBE_DIR=/abs/new-dir session-up.sh      (stop with session-down.sh)
# The directory is created with mode 0700; an existing one is accepted only if it is ours and 0700.
# API_PROBE_COMPOSITOR overrides the nested compositor command (default: niri with a minimal config).
# API_PROBE_WESTON overrides the Weston command; API_PROBE_WESTON_RENDERER=pixman when the host has no usable EGL.
set -u
here=$(cd "$(dirname "$0")" && pwd); . "$here/env.sh" || exit 1
if [ ! -e "$API_PROBE_DIR" ] && [ ! -L "$API_PROBE_DIR" ]; then
  mkdir -p -m 700 -- "$API_PROBE_DIR" || exit 1
fi
probe_private_dir || exit 1
if [ -n "$(probe_units)" ]; then echo "this run is already up: $PROBE_UNIT_PREFIX" >&2; exit 1; fi
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME" "$XDG_STATE_HOME" "$API_PROBE_DIR/logs"
cat > "$API_PROBE_DIR/niri-nested.kdl" <<'KDL'
// Nested instance for measurements: no autostart, D-Bus interfaces on the private bus,
// no hot corner (synthetic pointer moves would open the overview).
debug {
    dbus-interfaces-in-non-session-instances
}
hotkey-overlay {
    skip-at-startup
}
prefer-no-csd
gestures {
    hot-corners {
        off
    }
}
KDL
compositor=${API_PROBE_COMPOSITOR:-niri -c $API_PROBE_DIR/niri-nested.kdl}
inner='
umask 077
dbus-daemon --session --address="$DBUS_SESSION_BUS_ADDRESS" --nofork --nopidfile >"$API_PROBE_DIR/logs/base.bus.log" 2>&1 &
dbus-daemon --session --address="$DBUS_SYSTEM_BUS_ADDRESS" --nofork --nopidfile >"$API_PROBE_DIR/logs/base.sysbus.log" 2>&1 &
${API_PROBE_WESTON:-weston} --backend=headless --renderer="${API_PROBE_WESTON_RENDERER:-gl}" --socket="$PROBE_HOST_DISPLAY" --width=1920 --height=1200 --idle-time=0 >"$API_PROBE_DIR/logs/base.weston.log" 2>&1 &
for i in $(seq 50); do [ -S "$XDG_RUNTIME_DIR/$PROBE_HOST_DISPLAY" ] && break; sleep 0.1; done
WAYLAND_DISPLAY=$PROBE_HOST_DISPLAY '"$compositor"' >"$API_PROBE_DIR/logs/base.compositor.log" 2>&1 &
for i in $(seq 50); do [ -S "$XDG_RUNTIME_DIR/$PROBE_DISPLAY" ] && break; sleep 0.1; done
# Bus-activated services (portal backends) need the display in the activation environment.
WAYLAND_DISPLAY=$PROBE_DISPLAY dbus-update-activation-environment WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
wait'
$PROBE_REAL_ENV setsid systemd-run --user --scope --quiet --collect --unit="$PROBE_UNIT_PREFIX-base" -p MemoryMax=3G -- \
  env XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" DBUS_SESSION_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS" bash -c "$inner" >/dev/null 2>&1 &
for i in $(seq 100); do [ -S "$XDG_RUNTIME_DIR/$PROBE_DISPLAY" ] && { echo "up: $XDG_RUNTIME_DIR/$PROBE_DISPLAY (units $PROBE_UNIT_PREFIX-*)"; exit 0; }; sleep 0.1; done
echo "compositor socket did not appear; see $API_PROBE_DIR/logs" >&2; exit 1
