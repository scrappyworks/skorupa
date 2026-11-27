#!/usr/bin/env python3
"""List Wayland globals without binding any of them (wl_display.get_registry + sync only)."""
import os, socket, struct, sys
path = sys.argv[1]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.connect(path)
def msg(obj, op, payload): return struct.pack('<IHH', obj, op, 8 + len(payload)) + payload
s.sendall(msg(1, 1, struct.pack('<I', 2)) + msg(1, 0, struct.pack('<I', 3)))  # get_registry(2), sync(3)
buf = b''; out = []; done = False
while not done:
    d = s.recv(65536)
    if not d: break
    buf += d
    while len(buf) >= 8:
        obj, op, size = struct.unpack_from('<IHH', buf)
        if len(buf) < size: break
        body, buf = buf[8:size], buf[size:]
        if obj == 2 and op == 0:
            name, slen = struct.unpack_from('<II', body)
            iface = body[8:8 + slen - 1].decode(); pad = (slen + 3) & ~3
            ver, = struct.unpack_from('<I', body, 8 + pad)
            out.append((iface, ver))
        elif obj == 3: done = True
for i, v in sorted(out): print(f"{i}\t{v}")
