#!/usr/bin/env python3
"""Summarise a WAYLAND_DEBUG=1 client log: globals bound (with version) and requests per interface.

usage: wl-summary.py CLIENT.wl.log [ADVERTISED.tsv]   -> JSON on stdout
       wl-summary.py --print SUMMARY.json              -> one-line overview
Understands the libwayland format (`iface#id.request(`) and the wayland-rs one (`[rs] -> iface@id.`).
"""
import collections, json, re, sys

LINE = re.compile(r'\]\s*(?:\[rs\]\s*)?(?:\{[^}]*\}\s*)?(discarded )?(-> )?([a-z_0-9]+)[#@](\d+)\.([a-z_0-9]+)\(')
BIND = re.compile(r'wl_registry[#@]\d+\.bind\((\d+), (?:Some\()?"([a-z_0-9]+)"\)?, (\d+)')


def summarise(log, advertised):
    binds = {}
    requests = collections.defaultdict(collections.Counter)
    events = collections.defaultdict(collections.Counter)
    count = 0
    with open(log, errors='replace') as f:
        for line in f:
            m = LINE.search(line)
            if not m:
                continue
            count += 1
            discarded, outgoing, iface, _id, member = m.groups()
            if outgoing:
                requests[iface][member] += 1
                b = BIND.search(line)
                if b:
                    binds[b.group(2)] = max(int(b.group(3)), binds.get(b.group(2), 0))
            elif not discarded:
                events[iface][member] += 1
    return {
        'messages': count,
        'binds': {k: binds[k] for k in sorted(binds)},
        'advertised': advertised,
        'not_bound': sorted(set(advertised) - set(binds)),
        'requests': {k: dict(v) for k, v in sorted(requests.items())},
        'events': {k: dict(v) for k, v in sorted(events.items())},
    }


def main():
    if sys.argv[1] == '--print':
        s = json.load(open(sys.argv[2]))
        adv = s.get('advertised', {})
        parts = [f'{k}:{v}' + ('' if str(v) == str(adv.get(k, v)) else f'/{adv[k]}') for k, v in s['binds'].items()]
        print(f"{s['messages']} messages, {len(s['binds'])} globals bound: " + ', '.join(parts))
        return
    advertised = {}
    if len(sys.argv) > 2:
        for line in open(sys.argv[2]):
            iface, version = line.rstrip('\n').split('\t')
            advertised[iface] = int(version)
    json.dump(summarise(sys.argv[1], advertised), sys.stdout, indent=1)


if __name__ == '__main__':
    main()
