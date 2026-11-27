#!/usr/bin/env python3
"""Call desktop portals on the private session bus and print one outcome line per call.

usage: portal-exerciser.py [filechooser]
Run it through run-client.sh so that the bus is the private one. It refuses the real session bus.
Screenshot is requested non-interactively; FileChooser (optional argument) opens a dialog and is
expected to stay unanswered. OpenURI is left out on purpose: it would start an application.
"""
import os
import sys

from gi.repository import Gio, GLib

NAME = 'org.freedesktop.portal.Desktop'
PATH = '/org/freedesktop/portal/desktop'
WAIT_MS = 7000
V = GLib.Variant

if f'/run/user/{os.getuid()}/' in os.environ.get('DBUS_SESSION_BUS_ADDRESS', f'/run/user/{os.getuid()}/'):
    sys.exit('refusing to run on the real session bus; use run-client.sh')

bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
loop = GLib.MainLoop()
counter = 0


def call(iface, method, params):
    return bus.call_sync(NAME, PATH, 'org.freedesktop.portal.' + iface, method, params, None,
                         Gio.DBusCallFlags.NONE, 8000, None)


def request(iface, method, build):
    """Call a method that returns a Request and wait for its Response. build(token) -> parameters."""
    global counter
    counter += 1
    token = f'apiprobe{counter}'
    sender = bus.get_unique_name()[1:].replace('.', '_')
    request_path = f'{PATH}/request/{sender}/{token}'
    state = {}

    def on_response(_c, _s, _p, _i, _sig, params):
        state['response'] = params.unpack()
        loop.quit()

    def on_timeout():
        state['timeout'] = True
        loop.quit()
        return False

    sub = bus.signal_subscribe(NAME, 'org.freedesktop.portal.Request', 'Response', request_path, None,
                               Gio.DBusSignalFlags.NONE, on_response)
    try:
        call(iface, method, build(token))
    except GLib.Error as e:
        bus.signal_unsubscribe(sub)
        print(f'{iface}.{method}: ERROR {e.message[:160]}')
        return None
    timer = GLib.timeout_add(WAIT_MS, on_timeout)
    loop.run()
    bus.signal_unsubscribe(sub)
    if 'timeout' not in state:
        GLib.source_remove(timer)
    response = state.get('response')
    if response is None:
        print(f'{iface}.{method}: no response in {WAIT_MS} ms')
        return None
    code = {0: 'ok', 1: 'cancelled', 2: 'ended'}.get(response[0], response[0])
    print(f'{iface}.{method}: response={code} keys={sorted(response[1].keys())}')
    return response


def plain(label, fn):
    try:
        print(f'{label}: {fn()}')
    except GLib.Error as e:
        print(f'{label}: ERROR {e.message[:160]}')


plain('Settings.ReadAll', lambda: 'namespaces=' + ','.join(sorted(call('Settings', 'ReadAll', V('(as)', ([],))).unpack()[0])))
plain('Notification.AddNotification', lambda: (call('Notification', 'AddNotification', V('(sa{sv})', (
    'apiprobe', {'title': V('s', 'api-probe'), 'body': V('s', 'portal notification')}))), 'ok')[1])
request('Inhibit', 'Inhibit', lambda t: V('(sua{sv})', ('', 8, {'handle_token': V('s', t), 'reason': V('s', 'api-probe')})))
request('Screenshot', 'Screenshot', lambda t: V('(sa{sv})', ('', {'handle_token': V('s', t), 'interactive': V('b', False)})))
request('Camera', 'AccessCamera', lambda t: V('(a{sv})', ({'handle_token': V('s', t)},)))

created = request('ScreenCast', 'CreateSession', lambda t: V('(a{sv})', (
    {'handle_token': V('s', t), 'session_handle_token': V('s', 'apiprobesession')},)))
if created and created[0] == 0:
    session = created[1]['session_handle']
    request('ScreenCast', 'SelectSources', lambda t: V('(oa{sv})', (session, {
        'handle_token': V('s', t), 'types': V('u', 1), 'multiple': V('b', False), 'persist_mode': V('u', 0)})))
    started = request('ScreenCast', 'Start', lambda t: V('(osa{sv})', (session, '', {'handle_token': V('s', t)})))
    if started:
        print('   ScreenCast streams:', started[1].get('streams'))

# Interfaces that need a backend the session may not have: report whether they are exported at all.
for iface in ('GlobalShortcuts', 'RemoteDesktop', 'InputCapture', 'Wallpaper', 'Background', 'Secret', 'Clipboard', 'Usb'):
    plain(iface, lambda: 'exported, version ' + str(bus.call_sync(
        NAME, PATH, 'org.freedesktop.DBus.Properties', 'Get', V('(ss)', ('org.freedesktop.portal.' + iface, 'version')),
        None, Gio.DBusCallFlags.NONE, 3000, None).unpack()[0]))

if 'filechooser' in sys.argv[1:]:
    request('FileChooser', 'OpenFile', lambda t: V('(ssa{sv})', ('', 'api-probe open', {'handle_token': V('s', t)})))
