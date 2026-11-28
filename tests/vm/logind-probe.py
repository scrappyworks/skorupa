import array
import json
import os
import socket
import sys
import time
from pathlib import Path

import dbus

PATH = Path("/run/probe")
SERVICE = "org.freedesktop.login1"
MANAGER = SERVICE + ".Manager"
if sys.argv[1] == "monitor":
    from dbus.mainloop.glib import DBusGMainLoop
    from gi.repository import GLib

    DBusGMainLoop(set_as_default=True)
bus = dbus.SystemBus()
manager = bus.get_object(SERVICE, "/org/freedesktop/login1")
api = dbus.Interface(manager, MANAGER)
properties = dbus.Interface(manager, "org.freedesktop.DBus.Properties")


def inhibit():
    return api.Inhibit("sleep", "skorupa-probe", "REQ-LOCK-027 probe", "delay").take()


def notify(message, fd=None):
    with socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as sock:
        sock.connect(os.environ["NOTIFY_SOCKET"].replace("@", "\0", 1))
        ancillary = [] if fd is None else [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", [fd]))]
        sock.sendmsg([message.encode()], ancillary)


def write(name, value):
    temp = PATH / (name + ".tmp")
    temp.write_text(str(value))
    temp.replace(PATH / name)


def worker():
    inherited = int(os.environ.get("LISTEN_FDS", "0"))
    fd = 3 if inherited else inhibit()
    if not inherited:
        notify("FDSTORE=1\nFDNAME=sleep\nFDPOLL=0", fd)
    write("ready", f"{os.getpid()} {inherited}")
    for _ in range(300):
        if (PATH / "release").exists():
            notify("FDSTOREREMOVE=1\nFDNAME=sleep")
            os.close(fd)
            write("released", "yes")
            break
        time.sleep(0.1)
    while True:
        time.sleep(1)


command = sys.argv[1]
if command == "worker":
    worker()
elif command == "monitor":
    def prepare(pending):
        write("signal-" + str(bool(pending)).lower(), json.dumps({
            "pending": bool(properties.Get(MANAGER, "PreparingForSleep")),
            "suspend_started": (PATH / "suspend").exists(),
        }))

    bus.add_signal_receiver(prepare, signal_name="PrepareForSleep", dbus_interface=MANAGER,
                            bus_name=SERVICE, path="/org/freedesktop/login1")
    write("monitor", "ready")
    GLib.MainLoop().run()
elif command == "pending":
    sys.exit(0 if properties.Get(MANAGER, "PreparingForSleep") else 1)
elif command == "preflight":
    value = int(properties.Get(MANAGER, "InhibitDelayMaxUSec"))
    print(value)
    sys.exit(0 if value >= 2_000_000 else 1)
elif command == "listed":
    assert any(who == "skorupa-probe" and mode == "delay" for _, who, _, mode, _, _ in api.ListInhibitors())
elif command == "inhibit":
    try:
        fd = inhibit()
    except dbus.exceptions.DBusException as error:
        print(json.dumps({"dbus_error": error.get_dbus_name()}))
    else:
        os.close(fd)
        print(json.dumps({"granted": True}))
