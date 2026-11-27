# Private environment for API probes. Source it; never run clients without it.
# Needs API_PROBE_DIR: an absolute work directory outside the real runtime directory.
# Everything a run creates is private to the user (umask 077), and every transient unit
# carries a prefix derived from API_PROBE_DIR, so one run never touches another run's units.
umask 077
case "${API_PROBE_DIR:?set API_PROBE_DIR to an absolute work directory}" in
  /*) ;;
  *) echo "API_PROBE_DIR must be absolute" >&2; return 1 2>/dev/null || exit 1 ;;
esac
API_PROBE_DIR=$(realpath -m -- "$API_PROBE_DIR")
case "$API_PROBE_DIR" in
  /|/run/user/*|"$(realpath -m -- "$HOME")") echo "API_PROBE_DIR must not be the real runtime or home directory" >&2; return 1 2>/dev/null || exit 1 ;;
esac
export API_PROBE_DIR
export PROBE_UNIT_PREFIX=api-probe-$(printf %s "$API_PROBE_DIR" | sha256sum | cut -c1-12)
# The real user manager is only used to create and stop this run's transient scopes.
export PROBE_REAL_ENV="env XDG_RUNTIME_DIR=/run/user/$(id -u) DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/$(id -u)/bus"
export XDG_RUNTIME_DIR=$API_PROBE_DIR/run
export HOME=$API_PROBE_DIR/home
export XDG_CONFIG_HOME=$HOME/.config XDG_DATA_HOME=$HOME/.local/share XDG_CACHE_HOME=$HOME/.cache XDG_STATE_HOME=$HOME/.local/state
export DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/bus
# Stand-in system bus: no services, so every system call is recorded and fails.
export DBUS_SYSTEM_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/sysbus
unset WAYLAND_DISPLAY DISPLAY WAYLAND_SOCKET NIRI_SOCKET XDG_ACTIVATION_TOKEN DESKTOP_STARTUP_ID SSH_AUTH_SOCK
unset XDG_SESSION_ID XDG_SEAT XDG_VTNR INVOCATION_ID JOURNAL_STREAM NOTIFY_SOCKET
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=${API_PROBE_DESKTOP:-niri}
export PROBE_HOST_DISPLAY=api-probe-host
export PROBE_DISPLAY=${API_PROBE_DISPLAY:-wayland-1}

# The work directory holds raw Wayland and D-Bus traffic. It must be a real directory that we own
# and that nobody else can enter; a directory that was ever wider is refused, not repaired.
probe_private_dir() {
  if [ -L "$API_PROBE_DIR" ] || [ ! -d "$API_PROBE_DIR" ] || [ ! -O "$API_PROBE_DIR" ]; then
    echo "API_PROBE_DIR is not a directory owned by this user: $API_PROBE_DIR" >&2; return 1
  fi
  if [ "$(stat -c %a -- "$API_PROBE_DIR")" != 700 ]; then
    echo "API_PROBE_DIR must have mode 0700 from creation; use a new directory: $API_PROBE_DIR" >&2; return 1
  fi
}
# Units of this run only.
probe_units() {
  $PROBE_REAL_ENV systemctl --user list-units --plain --no-legend "$PROBE_UNIT_PREFIX-*.scope" | awk '{print $1}'
}
