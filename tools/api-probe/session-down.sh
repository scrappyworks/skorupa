#!/bin/bash
# Stop what session-up.sh and run-client.sh started for this API_PROBE_DIR, and nothing else:
# only scopes with this run's unit prefix, and only FUSE mounts inside this run's own runtime
# directory (the document portal and gvfs leave them behind).
set -u
here=$(cd "$(dirname "$0")" && pwd); . "$here/env.sh" || exit 1
probe_private_dir || exit 1
for u in $(probe_units); do $PROBE_REAL_ENV systemctl --user kill --signal=TERM "$u" 2>/dev/null; done
sleep 2
for u in $(probe_units); do $PROBE_REAL_ENV systemctl --user kill --signal=KILL "$u" 2>/dev/null; done
if [ -d "$XDG_RUNTIME_DIR" ] && [ ! -L "$XDG_RUNTIME_DIR" ] && [ -O "$XDG_RUNTIME_DIR" ]; then
  awk -v d="$XDG_RUNTIME_DIR/" 'index($2, d) == 1 {print $2}' /proc/mounts | while read -r m; do fusermount3 -u -z -- "$m"; done
fi
echo "stopped $PROBE_UNIT_PREFIX; private data remains in $API_PROBE_DIR"
