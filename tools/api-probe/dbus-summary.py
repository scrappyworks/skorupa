#!/usr/bin/env python3
"""Summarise dbus-monitor logs of one client run.

usage: dbus-summary.py SESSION.dbus SYSTEM.dbus   -> JSON on stdout
Per bus: method calls (destination, path, interface.member), names requested, names started,
names watched or queried, signal subscriptions, and error replies by name.
"""
import collections, json, re, sys

CALL = re.compile(r'^method call .*sender=(\S+) -> destination=(\S+) serial=\d+ path=([^;]+); interface=([^;]+); member=(\S+)')
KEEP = ('interface', 'member', 'sender', 'path')


def summarise(path):
    calls = collections.Counter()
    errors = collections.Counter()
    owned, started, watched, subscriptions = set(), set(), set(), set()
    try:
        lines = open(path, errors='replace').read().split('\n')
    except FileNotFoundError:
        return None
    for i, line in enumerate(lines):
        m = CALL.match(line)
        if m:
            _sender, dest, obj, iface, member = m.groups()
            if dest == 'org.freedesktop.DBus':
                arg = re.match(r'string "(.*)"', lines[i + 1].strip() if i + 1 < len(lines) else '')
                if not arg:
                    continue
                value = arg.group(1)
                if member == 'RequestName':
                    owned.add(value)
                elif member == 'StartServiceByName':
                    started.add(value)
                elif member in ('GetNameOwner', 'NameHasOwner'):
                    watched.add(value)
                elif member == 'AddMatch':
                    owner = re.search(r"arg0='([^']*)'", value)
                    if 'NameOwnerChanged' in value and owner:
                        watched.add(owner.group(1))
                    elif 'interface=' in value:
                        fields = [x for x in value.replace("'", '').split(',') if x.split('=')[0] in KEEP and not x.startswith('sender=:')]
                        subscriptions.add(' '.join(sorted(fields)))
                continue
            if iface != 'org.freedesktop.DBus.Monitoring':
                calls[f'{dest} {obj} {iface}.{member}'] += 1
        elif line.startswith('error '):
            e = re.search(r'error_name=(\S+)', line)
            if e:
                errors[e.group(1)] += 1
    return {'calls': dict(sorted(calls.items())), 'owned': sorted(owned), 'started': sorted(started),
            'watched': sorted(watched), 'subscriptions': sorted(subscriptions), 'errors': dict(errors)}


if __name__ == '__main__':
    out = {}
    for bus, path in zip(('session', 'system'), sys.argv[1:3]):
        result = summarise(path)
        if result is not None:
            out[bus] = result
    json.dump(out, sys.stdout, indent=1)
