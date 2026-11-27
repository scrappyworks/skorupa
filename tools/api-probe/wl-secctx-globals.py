#!/usr/bin/env python3
"""List the globals a sandboxed client sees.

usage: wl-secctx-globals.py COMPOSITOR_SOCKET
Creates a wp_security_context_v1 listener on the compositor, connects through it and prints
`interface<TAB>version` lines. The listening socket lives in a fresh directory that this script
creates with mode 0700 and removes again; it never touches a path it did not create.
"""
import array
import os
import socket
import struct
import sys
import tempfile
import time


def msg(obj, opcode, payload=b''):
    return struct.pack('<IHH', obj, opcode, 8 + len(payload)) + payload


def wl_string(text):
    data = text.encode() + b'\0'
    return struct.pack('<I', len(data)) + data + b'\0' * ((4 - len(data) % 4) % 4)


def globals_of(sock):
    """wl_display.get_registry (object 2) and sync (object 3); no bind."""
    sock.sendall(msg(1, 1, struct.pack('<I', 2)) + msg(1, 0, struct.pack('<I', 3)))
    buf = b''
    found = {}
    while True:
        data = sock.recv(65536)
        if not data:
            return found
        buf += data
        while len(buf) >= 8:
            obj, opcode, size = struct.unpack_from('<IHH', buf)
            if len(buf) < size:
                break
            body, buf = buf[8:size], buf[size:]
            if obj == 2 and opcode == 0:
                name, length = struct.unpack_from('<II', body)
                iface = body[8:8 + length - 1].decode()
                version, = struct.unpack_from('<I', body, 8 + ((length + 3) & ~3))
                found[iface] = (name, version)
            elif obj == 3:
                return found


def main():
    compositor = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    compositor.connect(sys.argv[1])
    available = globals_of(compositor)
    if 'wp_security_context_manager_v1' not in available:
        sys.exit('compositor does not offer wp_security_context_manager_v1 to this client')
    name, _version = available['wp_security_context_manager_v1']

    private_dir = tempfile.mkdtemp(prefix='api-probe-secctx-')  # mode 0700, owned by us
    listen_path = os.path.join(private_dir, 'listen')
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        listener.bind(listen_path)  # fails if the path exists; nothing is unlinked beforehand
        listener.listen(4)
        close_read, _close_write = os.pipe()  # the write end stays open until we exit
        # registry.bind -> object 4; manager.create_listener -> object 5 with the two descriptors
        compositor.sendall(msg(2, 0, struct.pack('<I', name) + wl_string('wp_security_context_manager_v1')
                               + struct.pack('<II', 1, 4)))
        compositor.sendmsg([msg(4, 1, struct.pack('<I', 5))],
                           [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array('i', [listener.fileno(), close_read]))])
        compositor.sendall(msg(5, 1, wl_string('org.example.api-probe')) + msg(5, 2, wl_string('org.example.TestApp'))
                           + msg(5, 3, wl_string('instance-1')) + msg(5, 4) + msg(1, 0, struct.pack('<I', 6)))
        time.sleep(0.5)
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.connect(listen_path)
        for iface, (_name, version) in sorted(globals_of(client).items()):
            print(f'{iface}\t{version}')
    finally:
        listener.close()
        if os.path.lexists(listen_path):
            os.unlink(listen_path)  # created by bind() above, inside our private directory
        os.rmdir(private_dir)


if __name__ == '__main__':
    main()
