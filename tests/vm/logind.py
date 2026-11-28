"""REQ-LOCK-026/027 and REQ-OPS-020 with the real logind D-Bus service."""

import json

machine.start()
machine.wait_for_unit("systemd-logind.service")
machine.wait_for_unit("user@1000.service")
user = "runuser -u probe -- env XDG_RUNTIME_DIR=/run/user/1000 systemctl --user"
probe = "runuser -u probe -- /run/current-system/sw/bin/skorupa-logind-probe"

machine.succeed(f"{probe} preflight")
machine.succeed(f"{user} start hold.service")
machine.wait_until_succeeds("test -s /run/probe/ready")
first_pid, inherited = machine.succeed("cat /run/probe/ready").split()
assert inherited == "0"
machine.wait_until_succeeds(f"test \"$({user} show hold.service -p NFileDescriptorStore --value)\" = 1")
machine.succeed(f"{probe} listed")

machine.succeed("systemd-run --unit=probe-monitor /run/current-system/sw/bin/skorupa-logind-probe monitor")
machine.wait_until_succeeds("test -e /run/probe/monitor")
machine.succeed("touch /run/probe/fail")
machine.succeed("systemctl suspend --no-block")
machine.wait_until_succeeds(f"{probe} pending")
pending_inhibit = json.loads(machine.succeed(f"{probe} inhibit"))
assert pending_inhibit == {"dbus_error": "org.freedesktop.login1.OperationInProgress"}
machine.succeed(f"{user} kill --signal=KILL --kill-whom=main hold.service")
machine.wait_until_succeeds(f"test \"$(cat /run/probe/ready)\" != '{first_pid} 0'")
new_pid, inherited = machine.succeed("cat /run/probe/ready").split()
assert first_pid != new_pid
assert inherited == "1"
machine.succeed(f"{probe} listed")
machine.succeed(f"{probe} pending")
machine.fail("test -e /run/probe/suspend")

machine.succeed("touch /run/probe/release")
machine.wait_until_succeeds("test -e /run/probe/released")
machine.wait_until_succeeds("test -e /run/probe/suspend")
machine.wait_until_fails(f"{probe} pending")
machine.fail("systemctl is-active --quiet systemd-suspend.service")
machine.wait_until_succeeds("test -e /run/probe/signal-false")
machine.succeed("test -e /run/probe/signal-true")
end = json.loads(machine.succeed("cat /run/probe/signal-false"))
assert end == {"pending": False, "suspend_started": True}
print("REQ-LOCK-026 failed suspend sends PrepareForSleep(false) after the suspend job.")
print("REQ-LOCK-027 retained descriptor across SIGKILL and released it on request.")

machine.succeed("mkdir -p /run/systemd/logind.conf.d")
machine.succeed("printf '[Login]\\nInhibitDelayMaxSec=1\\n' > /run/systemd/logind.conf.d/50-probe.conf")
machine.succeed("systemctl restart systemd-logind.service")
status, output = machine.execute(f"{probe} preflight")
assert status == 1 and int(output) == 1_000_000
print("REQ-OPS-020 preflight rejected a 1-second delay.")
