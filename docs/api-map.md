# External API map

Measured on 2026-11-27. This is the list of external interfaces Skorupa has to offer, broker or
deliberately refuse so that current applications and an ordinary scripted desktop keep working.

Status: **partial**. The Wayland, portal and session-bus layers are measured. The system layer is
measured only from the client side (calls attempted against an empty stand-in system bus); logind,
polkit and PAM behaviour itself comes from source. Gaps are listed in [Not measured](#not-measured).

## How to read the tables

Evidence marks:

- **M**: measured in the runs described under [Method](#method).
- **S**: read in source (niri `v26.04-324-g02ee0585`, the current Slint shell, the current locker).
- **K**: documented behaviour of a third-party tool, not traced here.

Decision column, using the trust domains of the design (`app`, `sandboxed`, `agent`, `shell`,
`portal`, `auth`):

- **all**: offered on the ordinary socket.
- **grant: X**: only connections of domain X see the global or may call the interface.
- **skip**: not offered in v1. **later**: planned after v1.
- **(proposed)**: the design does not name this interface; the decision is a proposal of this map.
- **(conflict)**: the design decision breaks a common desktop habit; see
  [Habits that conflict with the design](#habits-that-conflict-with-the-design).

Version column: the interface or specification version. Package versions are given only for the
components under test. `-` means the interface has no version of its own.

Test column: none of these exist yet as automated tests. The names refer to
[Planned checks](#planned-checks); `tools/api-probe/` holds the manual probes behind T1, T2 and T4.

## Method

- A private runtime directory, a private session bus and a second private bus used as a stand-in
  system bus (`DBUS_SYSTEM_BUS_ADDRESS`), both the reference `dbus-daemon`. A session that uses
  dbus-broker with systemd activation may activate services differently; that was not measured. No
  client talked to a real session or system bus. The stand-in system bus has no services, so every
  system call is recorded as an attempt and fails with `ServiceUnknown`.
- Compositor under test: niri at the revision above, nested through its winit backend inside
  `weston --backend=headless --renderer=gl` (Weston 16.0.0). Clients therefore saw the globals niri
  really offers, with dmabuf on a real render node, and nothing appeared on a live desktop. Weston
  headless itself has no `wl_seat`, so it was used directly only for a short comparison run.
- Each client ran 4 to 25 s with `WAYLAND_DEBUG=1` in its own memory-capped transient scope, with
  `dbus-monitor` on both private buses. Browsers were also driven with synthetic input inside the
  nested compositor (click, context menu, typing, copy, F11, Ctrl+O).
- A running niri session was touched once: a registry listing (`wl_display.get_registry` and
  `sync`, no bind) to compare its globals with the nested instance.
- A sandboxed client was simulated by creating a `wp_security_context_v1` listener and listing the
  registry through it.

Clients measured, named by engine or toolkit:

- Gecko: Firefox 160 (a nixpkgs build that fell back to software WebRender here) and a Gecko ESR
  browser on the hardware path.
- Chromium: a Chromium 158 browser started with `--ozone-platform=wayland --enable-wayland-ime`,
  and an Electron 45 application.
- GTK4: three GTK 4.24 applications (a terminal, an audio mixer, an image annotator).
- Qt6: `qml6`, a Qt Widgets application and a Qt polkit agent, all Qt 6.12.
- mpv 0.42 with four video outputs, foot 1.28, the current Slint shell.
- Tools: wl-clipboard 2.3, grim 1.5, slurp 1.5, wf-recorder 0.6, wlr-randr 0.5, swayidle 1.9,
  wtype 0.5, wlrctl 0.2, a GPU screen recorder in portal mode.
- Portals: xdg-desktop-portal 1.22.1 with the gtk 1.15.3 and wlr 0.8.4 backends, on a private
  `portals.conf` (gtk by default, wlr for ScreenCast and Screenshot).
- Libraries: Wayland 1.26.0, wayland-protocols 1.50.

The probe scripts are in `tools/api-probe/` (`session-up.sh`, `run-client.sh`, `session-down.sh`,
the two registry listers, the two log summarisers and `portal-exerciser.py`). They take the
compositor command from `API_PROBE_COMPOSITOR`, so the same runs can be repeated against Skorupa.
Raw logs contain window titles and bus traffic and stay outside the repository; the scripts keep
them in a directory that only the user can read.

## Wayland

niri advertises 47 globals in a tty session and 45 nested (M). The two missing nested are tty-only:
`zwlr_gamma_control_manager_v1` and `wp_drm_lease_device_v1`. A security-context client sees 29 of
the 45 (M).

"Bound by" gives the version each client bound. "Gecko", "Chromium", "GTK4" and "Qt6" are the groups
from [Method](#method) unless a row says otherwise.

### Offered to every client today

| Interface | niri ver. (M) | Bound by (M) | Requests seen (M) | Skorupa decision | Test |
| --- | --- | --- | --- | --- | --- |
| `wl_compositor` | 6 | Gecko 4, Chromium 4, GTK4 6, Qt6 6, mpv 6, foot 6, shell 6, slurp 4, wl-clipboard 2 | `create_surface`, `create_region` | all | T1, T2 |
| `wl_subcompositor` | 1 | all toolkits, mpv, foot | `get_subsurface`; `set_desync`, `set_position` (Gecko, Chromium) | all | T1, T2 |
| `wl_shm` | 2 | every client; v2 only Qt6, mpv, foot | `create_pool`, `create_buffer`, `resize` | all | T1, T2 |
| `wl_output` | 4 | every client; Gecko and slurp 3 | `release` | all | T1 |
| `wl_seat` | 9 | Chromium, GTK4, Qt6, shell 9; Gecko, mpv, foot 8; wtype, wlrctl 7; swayidle, wl-clipboard 2; slurp 1 | `get_pointer`, `get_keyboard`, `get_touch` (all toolkits, shell) | all | T1, T2 |
| `wl_data_device_manager` | 3 | all toolkits, mpv, foot 3; wl-clipboard 1 | `set_selection` with 9 to 13 MIME offers after Ctrl+C (Gecko, Chromium) | all, through focus | T2, T3 |
| `xdg_wm_base` | 7 | Gecko, GTK4, mpv, foot 7; Chromium, Qt6 6; wl-clipboard 1 | toplevel: `set_title`, `set_app_id`, `set_min_size`, `set_max_size`, `set_parent`, `set_fullscreen`, `unset_fullscreen`, `set_maximized`; popups with a full positioner (context menus) | all | T2, T8 |
| `zxdg_decoration_manager_v1` | 2 | Qt6 2; Chromium, mpv, foot 1. Not bound by Gecko or GTK4 | `get_toplevel_decoration`, `set_mode` | all | T2 |
| `org_kde_kwin_server_decoration_manager` | 1 | Gecko, Chromium, GTK4, Qt6 (10 clients) | `create`, `request_mode` (Gecko, GTK4) | all (proposed). It is the only server-side decoration path GTK4 and Gecko use | T2 |
| `zwp_linux_dmabuf_v1` | 6 | GTK4, Qt6 QML, Gecko ESR, mpv gpu, screen recorder 6; mpv dmabuf-wayland 5; Chromium, Firefox, wlr portal backend, wf-recorder 4 | `get_default_feedback`, `get_surface_feedback`, `create_params`, `add`, `create_immed`, `set_sampling_device` (v6: GTK4, Qt6, Gecko ESR) | all | T1, T2 |
| `wp_viewporter` | 1 | all toolkits, mpv, foot, shell | `set_destination`; `set_source` (Chromium) | all | T2 |
| `wp_fractional_scale_manager_v1` | 1 | all toolkits, mpv, foot. Not the shell | `get_fractional_scale` | all | T2 |
| `wp_presentation` | 2 | Chromium, two GTK4 apps, mpv 2; Gecko ESR, GTK4 terminal, Qt6 QML 1 | `feedback` on nearly every frame (Chromium: 1070 of 1099 commits) | all | T2, T9 |
| `wp_cursor_shape_manager_v1` | 2 | GTK4, mpv, foot 2; Gecko, Chromium, Qt6 1 | `get_pointer`, `set_shape` | all | T2 |
| `xdg_activation_v1` | 1 | all toolkits, mpv, foot, shell, wl-clipboard | `get_activation_token`, `set_serial`, `set_surface`, `commit`, `activate` | all, token only from real interaction (conflict) | T2, T3 |
| `zwp_idle_inhibit_manager_v1` | 1 | Gecko, Chromium, GTK4, mpv | `create_inhibitor` (mpv, Chromium while a video plays) | all | T2 |
| `zwp_text_input_manager_v3` | 1 | Gecko, Chromium, Qt6, mpv, foot, two GTK4 apps | `get_text_input`; `set_cursor_rectangle`, `commit` (Chromium, GTK4 terminal). No `enable` seen: no input method was running | all, routed to the trusted input method | T2 |
| `zwp_primary_selection_device_manager_v1` | 1 | all toolkits, foot, wl-clipboard | `create_source`, `set_selection` (Gecko, Chromium) | all, through focus | T2, T3 |
| `zxdg_output_manager_v1` | 3 | 17 of 24 clients; foot, grim, slurp, wf-recorder 2; screen recorder 1 | `get_xdg_output` | all (proposed) | T1 |
| `zxdg_exporter_v2` | 1 | Gecko, Chromium, GTK4, Qt6 | `export_toplevel` before a file dialog (Gecko, Chromium) | all (proposed): parent handle for portal dialogs | T2, T4 |
| `zxdg_importer_v2` | 1 | Gecko, Chromium, GTK4, Qt6 | `import_toplevel`, `set_parent_of` (Chromium, for its in-process GTK file dialog) | all (proposed); grant: portal is enough if Chromium is switched to portal dialogs | T2, T4 |
| `zwp_pointer_gestures_v1` | 3 | Gecko, Chromium, GTK4 3; Qt6 1 | `get_swipe_gesture`, `get_pinch_gesture`, `get_hold_gesture` | all (proposed): touchpad pinch and swipe inside apps | T2 |
| `zwp_tablet_manager_v2` | 1 | Gecko, Chromium, GTK4, Qt6, mpv | `get_tablet_seat` | later (proposed): pen input; clients treat it as optional | T2 |
| `zwp_pointer_constraints_v1` | 1 | Gecko, Chromium | none in these runs | later (proposed): pointer lock for games and remote desktops | T2 |
| `zwp_relative_pointer_manager_v1` | 1 | Gecko, Chromium | none in these runs | later (proposed), together with pointer constraints | T2 |
| `zwp_keyboard_shortcuts_inhibit_manager_v1` | 1 | Gecko, Chromium, GTK4, Qt6 | none in these runs | all with a compositor escape bind (proposed) | T2, T3 |
| `ext_idle_notifier_v1` | 2 | swayidle 2; Chromium, Electron 1 | `get_idle_notification` | grant: shell (conflict). Browsers lose their idle-state API only | T1, T3 |
| `ext_background_effect_manager_v1` | 1 | GTK4, foot | `get_background_effect` | skip (proposed): blur behind a client surface | T1 |
| `mutter_x11_interop` | 1 | nobody | none | skip: stub for the GNOME portal backend (S) | T1 |
| `wp_drm_lease_device_v1` | 1 (tty backend only) | nobody | none | skip (proposed): VR headsets | T1 |

### Hidden from sandboxed clients today, visible to every other client

niri filters these 16 globals (17 with gamma) only for security-context clients (M: registry through
a security-context listener; S: `src/niri.rs:2644`). Every ordinary process of the user can bind
them today.

| Interface | niri ver. (M) | Bound by (M) | Requests seen (M) | Skorupa decision | Test |
| --- | --- | --- | --- | --- | --- |
| `zwlr_layer_shell_v1` | 5 | shell 4; slurp 1 | `get_layer_surface`; `set_size`, `set_anchor`, `set_exclusive_zone`, `set_keyboard_interactivity` | grant: shell (conflict) | T1, T3 |
| `ext_session_lock_manager_v1` | 1 | nobody in these runs; the locker binds it (S) | not measured | grant: auth, first bring-up only | T1, T3 |
| `ext_foreign_toplevel_list_v1` | 1 | grim, wlr portal backend | handle `destroy` only | grant: shell. `portal` gets no window list, only the one chosen window; see [Scoped capture lookup](#scoped-capture-lookup-for-the-portal) | T1, T3 |
| `ext_workspace_manager_v1` | 1 | nobody. The shell uses niri IPC instead (S) | none | grant: shell | T1, T3 |
| `ext_image_copy_capture_manager_v1` | 1 | grim, wlr portal backend | `create_session`, `create_frame`, `attach_buffer`, `damage_buffer`, `capture` | grant: portal, for the chosen source only; agent on its own desk (conflict) | T1, T3, T4 |
| `ext_output_image_capture_source_manager_v1` | 1 | grim, wlr portal backend | `create_source` | grant: portal, only when the chosen source is the output; agent on its own desk | T1, T3 |
| `ext_foreign_toplevel_image_capture_source_manager_v1` | 1 | grim, wlr portal backend | none (no window capture requested) | grant: portal, only for the chosen window | T1, T3 |
| `zwlr_screencopy_manager_v1` | 3 | wf-recorder 3 (the only user); grim 1 and the wlr portal backend 3 bind it but used image-copy-capture | `capture_output_region`, `copy_with_damage` (wf-recorder) | skip. grim 1.5 and the wlr portal 0.8.4 do not need it | T1 |
| `ext_data_control_manager_v1` | 1 | wl-clipboard, mpv | wl-clipboard: `create_data_source`, `set_selection`, `set_primary_selection`, `receive`; mpv: `get_data_device` | skip (conflict) | T1, T3 |
| `zwlr_data_control_manager_v1` | 2 | wl-clipboard (bound, unused: it prefers the `ext` version) | none | skip | T1 |
| `zwp_virtual_keyboard_manager_v1` | 1 | wtype | `create_virtual_keyboard`, `keymap`, `key` | agent desk only; RemoteDesktop later (conflict) | T1, T3 |
| `zwlr_virtual_pointer_manager_v1` | 2 | wlrctl | `create_virtual_pointer`, `motion`, `frame` | agent desk only | T1, T3 |
| `zwlr_foreign_toplevel_manager_v1` | 3 | wlrctl | list only | skip (conflict: activity trackers, K) | T1 |
| `zwp_input_method_manager_v2` | 1 | nobody: no input method was running | none | grant: shell (on-screen keyboard) | T1, T3 |
| `zwlr_output_manager_v1` | 4 | wlr-randr | read-only listing | skip; rotation and scale become compositor commands | T1 |
| `wp_security_context_manager_v1` | 1 | the probe only | `create_listener`, `set_sandbox_engine`, `set_app_id`, `set_instance_id`, `commit` | grant: sandbox launcher | T1, T3 |
| `zwlr_gamma_control_manager_v1` | 1 (tty backend only) | not measurable nested; gamma-based dimming daemons use it (K) | not measured | skip (conflict) | T1 |

### Scoped capture lookup for the portal

The design gives the portal only the selected source: the choice is made in the compositor, restore
is off, and Stop is bound to the stream generation. The capture protocols do not have that shape by
themselves, so the grant has to be narrower than "the portal sees the global". The contract below
is a proposal of this map:

- **What the backend does today.** The wlr backend 0.8.4 binds `ext_foreign_toplevel_list_v1` and
  both capture source managers at start, before any request (M). A window source can only be
  created from a toplevel handle, and handles come only from that list. An unfiltered list announces
  every window with its title and app id (K: protocol semantics), which is the window list the
  design withholds.
- **Before a choice.** The `portal` connection sees the three capture globals and the list global,
  but the list announces no toplevel, and `create_source` or `create_session` for any output or
  window yields a session that is stopped at once and never delivers a frame.
- **After a choice.** The compositor records one grant: this portal connection, this request, this
  stream generation, this source. If the source is a window, the portal's list instance announces
  exactly that one handle, with its identifier and with empty title and app id. If the source is the
  output, the list stays empty and only the output source resolves. A region choice is cropped by
  the compositor before any buffer reaches the backend.
- **Stop.** The indicator's Stop, the portal's `Session.Close`, lock, and the window closing all
  revoke the grant: the capture session receives `stopped`, the handle receives `closed`, and no
  later frame is delivered. A new share needs a new choice.
- **Still open.** The handle and identifier format has to be checked against the chooser contract
  of the wlr backend 0.9 before T1 and T3 become acceptance tests. A per-connection filtered list is
  Skorupa code; a stock list implementation announces everything to every bound client (K).

### Not offered by niri, bound when a compositor offers them

niri at this revision creates none of these (S). The middle column is a short run of the same
clients directly against Weston 16 (M) or documented client behaviour (K).

| Interface | Who uses it | Skorupa decision | Test |
| --- | --- | --- | --- |
| `wp_linux_drm_syncobj_manager_v1` | Mesa EGL and Vulkan WSI, GTK4, Gecko (K). Weston headless does not offer it, so not measured | all (design: explicit sync in v1) | T2, T9 |
| `wp_single_pixel_buffer_manager_v1` | GTK4, mpv, Chromium, foot bind it on Weston (M); the locker binds it (S); niri builds it only for tests (S) | all (proposed) | T2 |
| `wp_fifo_manager_v1`, `wp_commit_timing_manager_v1` | mpv with Vulkan through Mesa WSI: `set_barrier`, `wait_barrier`, `set_timestamp` (M on Weston) | later (proposed); measure against the frame-pacing goal first | T9 |
| `wp_tearing_control_manager_v1` | bound by the Vulkan path (M on Weston), no request | skip (proposed) | T1 |
| `wp_color_representation_manager_v1` | GTK4, Gecko ESR, mpv bind it; mpv Vulkan sets coefficients and range (M on Weston) | later, with HDR and colour management | T1 |
| `wp_color_manager_v1`, `wp_content_type_manager_v1` | mpv, GTK4, Chromium (K) | later (design: colour after v1) | T1 |
| `xdg_wm_dialog_v1` | GTK4, Qt6 mark modal dialogs (K) | later (proposed): supports "dialogs stay with the parent" | T2 |
| `xdg_toplevel_icon_manager_v1`, `xdg_system_bell_v1`, `wp_alpha_modifier_v1` | optional in toolkits (K) | skip (proposed) | T1 |
| `zwp_text_input_manager_v1` | Chromium and Qt6 bind it on Weston (M); with v3 present Chromium used v3 on niri (M) | skip | T2 |
| `zwp_linux_explicit_synchronization_v1`, `wl_drm` | no measured client asked for either | skip | T1 |
| Xwayland shell and keyboard grab | X11 clients only | skip (no X11) | T1 |

## Portals

Frontend: xdg-desktop-portal 1.22.1 on the private bus with gtk as the default backend, wlr for
ScreenCast and Screenshot and a Secret Service provider for Secret. Versions are the `version`
property the frontend exported (M). With niri's stock portal file alone, which prefers the GNOME
backend, and only the gtk and wlr backends installed, the frontend exported neither ScreenCast nor
Screenshot (M), and none of niri's `org.gnome.*` interfaces had a caller.

| Interface | Ver. (M) | Backend in the runs | Who uses it | Skorupa decision | Test |
| --- | --- | --- | --- | --- | --- |
| `Settings` | 2 | gtk (reads dconf) | M: every toolkit at start. `Read` (GTK4, Electron), `ReadAll` (Gecko, Qt6), `ReadOne` (Qt Widgets app); `SettingChanged` subscriptions. 11 namespaces served, `org.freedesktop.appearance` among them | all; gtk backend, accent colour and dark mode | T4 |
| `FileChooser` | 4 | gtk | M: Gecko `OpenFile`, dialog window owned by the backend. Chromium did not call it and opened an in-process GTK dialog | all; gtk backend | T4 |
| `ScreenCast` | 4 | wlr 0.8.4 | M: the screen recorder in portal mode (`CreateSession`, `SelectSources`, `Start`, `OpenPipeWireRemote`). The backend used `ext-image-copy-capture`, returned one stream with a PipeWire node id and logged an empty `app_id` | all through the portal: wlr 0.9, chooser in the compositor, restore off, requester binding patch, [scoped lookup](#scoped-capture-lookup-for-the-portal) | T3, T4, T5 |
| `Screenshot` | 2 | wlr (runs grim) | M: `Screenshot` returned a `uri`; the backend wrote the image to the fixed path `/tmp/out.png`, mode 0644 | all; own small adapter | T4 |
| `Camera` | 1 | frontend, with `Access` and `PermissionStore` | M: `AccessCamera` led to `PermissionStore.Lookup` and `impl.portal.Access.AccessDialog` on the gtk backend, then waited for the dialog. The prompt carried an empty app id, and when the caller went away the frontend stored `no` for camera under that empty id | all; the consent dialog must come from the trusted UI, so an own `Access` backend, and no remembered decision without a verified requester (proposed) | T4 |
| `Notification` | 1 | gtk | M: `AddNotification` became `org.freedesktop.Notifications.Notify` on the shell | all, allowed silently | T4, T5 |
| `Inhibit` | 3 | gtk | M: `Inhibit` became `org.freedesktop.ScreenSaver.Inhibit` on the compositor, `UnInhibit` on close | all; needs an owner for `org.freedesktop.ScreenSaver` or an own `Inhibit` backend (open) | T4 |
| `OpenURI` | 5 | frontend with the gtk app chooser | not exercised (it would start a browser); S and K | all | T4 |
| `Secret` | 1 | Secret Service provider | M: interface exported; no measured client asked for a secret (browsers ran with a basic password store) | all (proposed): keep an existing secret service | T4 |
| `GlobalShortcuts` | absent | none (needs the GNOME backend) | M: `UnknownMethod` | skip | T4 |
| `RemoteDesktop`, `InputCapture` | absent | none | M: `UnknownMethod` | later (design) | T4 |
| `Background`, `Wallpaper`, `Usb`, `Clipboard` | absent | none | M: not exported | skip (proposed) | T4 |
| `NetworkMonitor` (3), `ProxyResolver` (1), `MemoryMonitor` (1), `PowerProfileMonitor` (1), `Realtime` (1), `GameMode` (4), `Trash` (1), `Account` (1), `Email` (4), `Print` (4), `DynamicLauncher` (1), `Location` (1) | as listed | frontend or gtk | M: exported. Only `NetworkMonitor.GetStatus` had a caller (Chromium) | all: whatever the stock frontend and gtk backend provide; no Skorupa code | T4 |
| `host.portal.Registry` | 1 | frontend | M: `Register` by Gecko, Chromium, Electron, GTK4 and the Qt Widgets app. The Chromium build registered itself as `org.chromium.Chromium` although its window app-id is a different name | all; a self-declared name, never an identity | T4 |
| `Documents` (own bus name), `impl.portal.PermissionStore` | 1 | xdg-document-portal, xdg-permission-store | M: started by the first GTK4 app (`GetMountPoint`); the permission store received `Lookup`, `SetPermission` and `Set` | all (stock) | T4 |

## D-Bus services a desktop provides

Session bus unless stated. "In the runs" is who owned the name in the private session.

| Name and interface | Version | In the runs | Callers | Skorupa decision | Test |
| --- | --- | --- | --- | --- | --- |
| `org.freedesktop.Notifications` | spec 1.2 (S) | shell (M) | M: `notify-send` (`Notify`), gtk portal backend (`GetServerInformation`, `Notify`); niri after a screenshot (S) | shell | T5 |
| `org.kde.StatusNotifierWatcher`, consuming `org.kde.StatusNotifierItem` and `com.canonical.dbusmenu` | - | shell (M) | tray applications (K) | shell | T5 |
| `org.freedesktop.ScreenSaver`: `Inhibit`, `UnInhibit` only | - | niri (M: introspected) | M: Chromium (`Inhibit`, and `GetActive`, which fails with `UnknownMethod`), Firefox (`Inhibit`; it did not create a Wayland inhibitor in that run), gtk portal backend. The Qt Widgets app subscribes to `ActiveChanged` here and on the GNOME and Xfce names | open: either a small bridge to the compositor's idle inhibit or skip and rely on `zwp_idle_inhibit` (Chromium uses both; Firefox falls back, K) | T5 |
| `org.freedesktop.portal.Desktop`, `.Documents`, `impl.portal.PermissionStore`, `impl.portal.desktop.gtk` and `.wlr` | frontend 1.22.1, gtk 1.15.3, wlr 0.8.4 (M) | stock packages, bus-activated | see Portals | stock; wlr backend patched; own `Screenshot` and `Access` backends in the `portal` domain | T4 |
| polkit agent: object registered with `org.freedesktop.PolicyKit1.Authority.RegisterAuthenticationAgent` (system bus) | - | a Qt polkit agent (M: the call, against the stand-in bus) | polkit, for any privileged action | grant: auth. D-Bus bridge in `lock/`, prompt in the trusted UI, no unlock right | T6 |
| `org.freedesktop.secrets` | Secret Service | a keyring daemon, bus-activated (M) | portal `Secret`, libsecret users such as mail clients and browsers (K) | keep external (proposed) | T5 |
| MPRIS: `org.mpris.MediaPlayer2.*` | MPRIS 2.2 | not exercised | M: the shell watches an MPRIS aggregator name and its `PropertiesChanged` | keep external; shell consumes | T5 |
| a gamma-based dimming daemon's bus name | - | not run | M: the shell watches the name | skip with gamma control (conflict) | T5 |
| `org.gnome.Mutter.ScreenCast`, `org.gnome.Mutter.DisplayConfig`, `org.gnome.Shell.Introspect`, `org.gnome.Shell.Screenshot`, `org.gnome.Shell` (accelerator grabs), `org.gnome.Mutter.ServiceChannel` | ScreenCast `Version` 5 (M) | niri (M: first five introspected on the private bus; the last needs a session instance, S) | only xdg-desktop-portal-gnome and GNOME Settings (S, K). M: no call from any measured client | skip | T5 |
| `org.freedesktop.a11y.Manager`: `KeyboardMonitor`, `PointerLocator` | - | niri | screen readers (S). M: an unrelated bus client called `WatchKeyboard` and received `KeyEvent` signals for keys typed in the nested session | skip. If accessibility is added later it needs a grant | T5 |
| `org.a11y.Bus` | - | at-spi launcher, bus-activated | M: Chromium, Electron and Gecko read its properties; Qt6 and Gecko watch it | skip until the accessibility position is decided | T5 |
| `org.gnome.Mutter.IdleMonitor`, `org.gnome.SessionManager` | - | nobody | M: Gecko calls `IdleMonitor.GetIdletime` (fails); Chromium and the Qt Widgets app watch `SessionManager` | skip; clients cope today | T5 |
| `org.gtk.vfs.*`, `ca.desrt.dconf` | - | gvfs, dconf, bus-activated | M: GTK file dialogs and settings writes | stock | none |

## D-Bus services consumed

System bus calls were made against the stand-in bus, so the table shows what clients ask for, not
what the services answer.

| Service (bus) | Version | Members | Who (evidence) | Skorupa decision | Test |
| --- | --- | --- | --- | --- | --- |
| `org.freedesktop.login1` (system) | - | `Manager.Inhibit`; `GetSession`, `GetSessionByPID`; signals `PrepareForSleep`, `Session.Lock`; property changes | M: swayidle (all of these), Chromium (`Inhibit`), the Qt Widgets app (`PrepareForSleep`), Gecko (property changes), niri (`LidClosed`, `PrepareForSleep`). S: shell (`Suspend`, `Session.Lock`, `SetBrightness`, `LockedHint`), niri (`SetLockedHint`, power-key inhibitor), locker (`CanSuspend` and friends) | all read it directly. Compositor: session, lid, sleep, idle lock. `lock/` bridge: `Lock`, `Unlock`, `LockedHint`, sleep delay inhibitor | T6 |
| `org.freedesktop.PolicyKit1` (system) | - | `Authority.RegisterAuthenticationAgent`, authority signals | M: the Qt polkit agent | `lock/` bridge | T6 |
| `org.freedesktop.UPower` (system) | - | properties, `DisplayDevice` changes | M: shell (watch), Chromium (`GetAll`) | shell consumes; apps read directly | T6 |
| `org.freedesktop.NetworkManager` (system) | - | properties, `Device.Wireless`, `Connection.Active`, `Settings.Connection` | M: shell (subscriptions), GTK4 network monitor | shell consumes | T6 |
| `org.bluez` (system) | - | `ObjectManager.GetManagedObjects`, property changes | M: shell (watch), Chromium, Electron | shell consumes | T6 |
| `org.freedesktop.resolve1`, a vendor platform daemon (system) | - | property changes | M: shell | shell consumes | T6 |
| `org.freedesktop.RealtimeKit1` (system) | - | properties | M: Gecko, the GTK4 terminal, PipeWire | stock | none |
| `org.freedesktop.hostname1`, `timedate1`, `UDisks2`, `UPower.PowerProfiles`, `network1` (system) | - | property watches | M: Chromium, Gecko, GTK4 | stock | none |
| `org.freedesktop.locale1` (system) | - | keyboard layout properties | M: niri at start | skip: layout comes from the config | none |
| `org.freedesktop.systemd1` (user) | - | `StartTransientUnit` per launched app | S: niri; the shell uses `systemd-run --user` | shell launcher; the unit name is what the portal frontend reads as app id (K) | T7 |
| `org.freedesktop.DBus` (session) | - | `UpdateActivationEnvironment` | M: until `WAYLAND_DISPLAY` was exported to the private bus, the gtk backend could not start and Gecko fell back to an in-process file dialog | session start must export the environment before any portal call | T4 |
| PipeWire (socket, not D-Bus) | - | screencast node, audio | M: the wlr backend created a stream on a private PipeWire; the screen recorder opened the remote | stock; indicator and Stop tie to the stream | T4 |

## System

From source unless marked.

| Interface | Version | Used for | Who | Skorupa decision | Test |
| --- | --- | --- | --- | --- | --- |
| logind session through libseat (`TakeControl`, `TakeDevice`, pause and resume) | - | DRM and input devices without root; VT switch | niri (S, K for the calls inside libseat) | compositor, through Smithay's libseat session | T6, hardware bench |
| logind inhibitors and sleep signals | - | lock before sleep, power key | swayidle (M), niri, shell, locker (S) | compositor and `lock/` bridge; an expired inhibitor must leave the session locked | T6 |
| udev | - | DRM hotplug, input device list, device origin rule for "physical" keys | niri (S) | compositor | hardware bench |
| libinput | - | keyboard, touchpad, touch, pen | niri (S) | stays in the compositor | T8 |
| DRM/KMS, GBM, render node | - | scanout, dmabuf import | niri; M: clients and the wlr backend used the render node | compositor; one audited module for the GPU constructor | T9 |
| `/dev/uinput` and direct input-device access | - | synthetic keys outside Wayland | uinput typing tools (K) | not a Skorupa interface; who may open input devices is a deployment precondition | deployment check |
| KMS capture helpers with `CAP_SYS_ADMIN` | - | screen recording without the compositor | some screen recorders in their non-portal modes (K) | bypasses capture exclusion; recording has to use the portal path | deployment check |
| PAM | - | password check | locker: own PAM services, `pam_acct_mgmt` kept (S) | `lock/` only | lock tests |
| polkit helper (`polkit-agent-helper-1`) | - | finishing a polkit authentication | polkit agents (K) | `lock/` bridge calls the helper | T6 |
| systemd user manager | - | `graphical-session.target`, `Type=notify`, socket activation, transient scopes per app | niri, shell, locker (S) | same mechanisms; no own supervisor | T7 |
| journald | - | structured logs | all | all; never log window titles or secrets (the wlr backend logs titles at INFO, design note) | review |

## Files

| Path or convention | Version | Used for | Who | Skorupa decision | Test |
| --- | --- | --- | --- | --- | --- |
| `$XDG_RUNTIME_DIR/wayland-N` and `.lock` | Wayland 1.26.0 (M) | client socket | every client (M) | all; domain sockets are passed as inherited descriptors, never looked up by name | T1 |
| `$XDG_RUNTIME_DIR/niri.<display>.<pid>.sock`, `$NIRI_SOCKET` | niri IPC (S) | compositor IPC, no peer check (S) | shell: event stream and one action (S); `niri msg` in user scripts | replaced by the `skorupa` CLI with `--json` and peer credentials | T7 |
| shell command socket and lock file in the runtime directory | - | shell CLI, usable from key binds (S); M: both files created in the nested run | shell | shell | T7 |
| `$XDG_RUNTIME_DIR/bus`, `pipewire-0`, `doc/`, `keyring/`, `dconf/`, `gvfs/` | - | session bus, PipeWire, document portal mount, secret service, settings | stock services (M: all appeared in the private runtime directory) | stock | none |
| `portals.conf` in `$XDG_CONFIG_HOME/xdg-desktop-portal/`, `<desktop>-portals.conf` and `portals/*.portal` in the system data directory, chosen by `XDG_CURRENT_DESKTOP` | frontend 1.22.1 (M) | backend selection | portal frontend (M: the private copy selected gtk and wlr) | ship `skorupa-portals.conf`; set `XDG_CURRENT_DESKTOP` | T4 |
| `xdg-desktop-portal-wlr/config` | 0.8.4 (M) | chooser type and command | wlr backend (M: `chooser_type=none` with an output name worked) | chooser in the compositor, `simple` type | T4 |
| `.desktop` files in `$XDG_DATA_DIRS/applications`, `mimeapps.list`, icon themes | Desktop Entry 1.5 (K) | launcher, default applications for `OpenURI`, app names and icons | shell (S), portal frontend (K) | shell and stock frontend | T7 |
| session entry in `wayland-sessions/` | - | greeter starts the session | greeter | one entry; niri stays as the fallback session | manual |
| D-Bus service files, systemd user units | - | activation of portals, secret service, shell, locker | stock (M: bus activation started the portal stack) | stock paths | T7 |
| xkb data, cursor themes, fontconfig | - | keymap, pointer images, fonts | compositor and shell | system files; `XCURSOR_THEME`, `XCURSOR_SIZE` for clients | T2 |
| dconf database (`org.gnome.desktop.interface` and others) | - | what `Settings` serves | gtk portal backend (M) | keep; dark mode and accent come from here or from `config.toml` (open) | T4 |
| `config.toml` | - | Skorupa settings, hot reloaded | compositor, shell | replaces the compositor configuration | T7 |
| `/tmp/out.png` | - | Screenshot result of the wlr backend (M) | wlr backend | removed with the own adapter; result goes to a private file or descriptor | T4 |
| `/etc/pam.d/<service>`, polkit action files | - | authentication policy | locker, polkit | `lock/` packaging | lock tests |
| environment for clients: `WAYLAND_DISPLAY`, `XDG_CURRENT_DESKTOP`, `XDG_SESSION_TYPE`, `XDG_ACTIVATION_TOKEN`, toolkit backend hints | - | session environment | set by the compositor and its config (S) | set by the compositor and the launcher | T2 |

## Habits that conflict with the design

A scripted wlroots-style desktop leans on interfaces that the design restricts. The list is kept
generic on purpose: it names the kind of tool and the interface it needs, not any particular setup.
The protocol evidence is in the tables above.

What the current shell needs is small and fits the design: layer-shell, `wl_shm`, seat, output,
`xdg_activation_v1` and viewporter (M); the `Notifications` and `StatusNotifierWatcher` names (M);
logind, UPower, NetworkManager, BlueZ, resolve1 and MPRIS as a consumer (M and S); and from the
compositor an event stream of workspace and window changes, "focus workspace by id" and quit (S).

| Habit | Needs | Design decision | Way out to decide |
| --- | --- | --- | --- |
| Software dimming below the backlight minimum with a gamma daemon | `zwlr_gamma_control_manager_v1` | skip | a compositor dim command, or gamma as grant: shell |
| Region selection with a layer-shell selector, in scripts and as a portal chooser | `zwlr_layer_shell_v1` from an `app` connection | grant: shell | selection UI in the compositor or shell, exposed through the CLI |
| Colour picking or screenshots with a command-line capture tool | image capture from an `app` connection (M: grim 1.5 prefers `ext-image-copy-capture`) | grant: portal only | compositor screenshot and colour-pick commands |
| Command-line clipboard tools in scripts with no focused window | `ext_data_control_manager_v1` (M) | skip | a CLI clipboard command brokered by the shell; without it such tools fall back to a focus-stealing surface, which the activation rule will refuse |
| Typing into a window from a key bind with a virtual-keyboard tool | `zwp_virtual_keyboard_manager_v1` on the user's seat (M) | agent desk only | an application-side mechanism |
| Typing through a uinput tool | kernel uinput | outside Wayland; "physical key" rule by device origin | classify uinput devices as synthetic |
| Screen recording through a KMS helper with `CAP_SYS_ADMIN` | direct KMS capture | capture exclusion and indicator live in the compositor | portal ScreenCast (M: a recorder's portal mode completed the handshake with the wlr backend; frames not measured) |
| Idle daemons and activity trackers outside the shell | `ext_idle_notifier_v1` (M), `zwlr_foreign_toplevel_manager_v1` (K) | grant: shell; skip | idle lock moves into the compositor (decided); a tracker needs a shell data provider |
| A shell that focuses windows without a valid activation serial | lenient `xdg_activation_v1` | token only from real interaction | the shell requests tokens with a real input serial |
| Scripts that query the compositor over its IPC, some parsing human-readable output | compositor IPC | CLI with `--json` | the CLI must cover outputs, windows, focus, screenshots, monitor power and quit |
| On-demand VRR, blur and opacity rules for inactive windows | compositor rendering | VRR "next"; blur not mentioned | accept the gap at start or add to v1 |
| Server-side decorations in GTK4 and Gecko applications | `org_kde_kwin_server_decoration_manager` (M) | not in the design table | offer it, or accept client-side title bars in those apps |
| Per-window and per-layer capture exclusion rules | compositor rule | named in the design only for trusted UI and notifications | state per-application exclusion as a requirement |

## Planned checks

- **T1 registry snapshot per domain.** Connect as `app`, `sandboxed` (security-context listener),
  `shell`, `portal`, `auth` and `agent`; list globals and versions without binding; compare with the
  decision column. `wl-globals.py` and `wl-secctx-globals.py` do this for niri today.
- **T2 client matrix.** Run the client list from [Method](#method) through `run-client.sh` against
  a headless Skorupa; every "all" interface must be bound at the version recorded here or higher,
  and no client may exit or log a missing global.
- **T3 denial and scope.** For every grant-only interface: absent from the registry of other
  domains, a request on a stale object after revoke is refused, and the stream or transfer ends.
  For the portal: zero toplevels announced before a choice, exactly the chosen one after it, none
  after Stop, and no frame for any source that was not chosen.
- **T4 portal conformance.** `portal-exerciser.py` on a private bus with Skorupa's portal file;
  expected outcome per interface as in the Portals table, including "no file at a fixed path" and a
  non-empty requester binding for ScreenCast.
- **T5 session-bus ownership.** On a private bus: which names are owned, their introspection, and
  that names marked skip are absent.
- **T6 system-bus mocks.** logind, polkit, UPower, NetworkManager and BlueZ as mock services on a
  private bus: sleep, lock, inhibitor expiry, agent registration, daemon restart.
- **T7 files and CLI.** Temporary home and runtime directories: sockets, peer credentials, config
  reload, `.desktop` parsing, CLI exit codes and JSON.
- **T8 protocol conformance.** WLCS or an equivalent suite for core, xdg-shell and input.
- **T9 presentation.** Frame pacing and presentation feedback against niri on the same hardware.

## Not measured

- No client was run with an input method, a pen, a touch screen or pointer lock, so `enable` on
  text-input, tablet events, touch events and pointer constraints are bound but unexercised.
- ScreenCast stopped at the PipeWire node. `Start` answered with one stream (node id, position,
  size), but no session manager was started in the private environment (it would have opened real
  audio and camera devices), so no frames flowed and the recorder wrote no file.
- Window capture through the portal was not requested, so the toplevel list and the window source
  manager were bound but unused.
- A session locker, a gamma daemon and an activity tracker were not run; their rows come from
  source or documentation.
- MPRIS players, tray items and `OpenURI` were not exercised.
- logind, polkit, UPower, NetworkManager and BlueZ were absent on the stand-in system bus; only the
  first call of each client is known.
- The nixpkgs Firefox build ran without GPU acceleration, so its dmabuf path is represented by the
  Gecko ESR build.
- Explicit sync could not be measured: neither niri nor Weston headless offers the protocol.

## Insights

1. **On niri every unsandboxed process can already do what Skorupa wants to gate.** A
   security-context client sees 29 globals, an ordinary client 45 (M). The 16 extra include layer
   shell, session lock, both data-control protocols, screen capture, virtual keyboard and pointer.
   Evidence: registry listing through a security-context listener; niri `src/niri.rs:2644`.
2. **niri's accessibility interface is a session-bus key logger.** A plain bus client called
   `org.freedesktop.a11y.KeyboardMonitor.WatchKeyboard` on the nested instance and received six
   `KeyEvent` signals for three typed keys (M). The handler only records the sender
   (`src/dbus/freedesktop_a11y.rs:158-166`). Skorupa should not carry this interface over. By
   source a session instance starts the same interface; no running session was probed.
3. **The design's protocol table misses interfaces that every toolkit binds.** xdg-foreign v2
   (portal dialog parents), the KDE server-decoration protocol (the only server-side decoration path
   for GTK4 and Gecko), pointer gestures, xdg-output, tablet, pointer constraints, relative pointer
   and keyboard-shortcuts inhibit are all bound by browsers and toolkits (M) and have no decision.
4. **Explicit sync is a v1 goal that niri does not have.** niri creates no
   `wp_linux_drm_syncobj_manager_v1` (S; absent from a tty session's registry, M). "As snappy as
   niri" is therefore reachable without it, and it cannot be compared against niri.
5. **`wlr-screencopy` can be skipped, data-control cannot be skipped silently.** grim 1.5 and
   xdg-desktop-portal-wlr 0.8.4 both chose `ext-image-copy-capture` on niri (M; backend log line
   "using ext_image_copy_capture"). wl-clipboard chose `ext_data_control_manager_v1` (M), and
   scripts commonly call it without a focused window. mpv binds data-control too (M).
6. **"The portal gets only the selected source" needs a filtered toplevel list.** The wlr backend
   binds the toplevel list at start (M), and a window source can only be made from a list handle.
   Granting the global as it stands would hand the portal every title and app id; the
   [scoped lookup](#scoped-capture-lookup-for-the-portal) is compositor code that has to exist
   before the capture rows can be tested.
7. **The wlr Screenshot backend writes to a world-readable fixed path.** One portal call left
   `/tmp/out.png`, mode 0644, on a shared temporary filesystem (M). This confirms the design's
   reason for an own adapter.
8. **The Camera portal's consent prompt is an ordinary GTK window.** `AccessCamera` went to
   `impl.portal.Access.AccessDialog` on the gtk backend (M). To draw it in the trusted UI Skorupa
   needs its own `Access` backend; the design lists only ScreenCast and Screenshot adapters. The
   prompt named no application, and the abandoned request was remembered as `no` under the empty
   app id, which is shared by every unsandboxed program (M).
9. **Requester identity at the portal is self-declared or empty.** The wlr backend logged
   `app_id:` empty for both ScreenCast clients, and a Chromium build registered itself with the host
   registry as `org.chromium.Chromium` while its window app-id is a different name (M). The
   requester binding patch has to take identity from the launcher's unit or connection, not from
   these fields.
10. **Idle inhibition needs an owner on the bus or a decision to drop it.** The gtk `Inhibit`
    backend, Chromium and Firefox all call `org.freedesktop.ScreenSaver.Inhibit`, which niri owns
    (M). Firefox did not create a Wayland inhibitor in the same run. The design gives the compositor
    no session-bus name.
11. **Portal backends fail quietly without the activation environment.** Before
    `WAYLAND_DISPLAY` was exported to the private bus the gtk backend never started and Gecko
    opened an in-process dialog instead of the portal one (M). Session start order is part of the
    contract.
12. **Chromium does not use the FileChooser portal by default** (M: in-process GTK dialog, with
    xdg-foreign export and import inside one process). Policies that rely on the portal seeing
    file access do not cover it unless the browser is switched to portal dialogs (K).
13. **Common scripted-desktop habits conflict with five "skip" or "shell only" decisions**: gamma
    for software dimming, layer shell for region selectors, image capture for command-line
    screenshot tools, data-control for clipboard scripts, virtual keyboard for typing binds. Each
    needs a replacement command before a switch; see the conflict table.
14. **A recorder that captures from KMS with `CAP_SYS_ADMIN` bypasses the compositor**, so no
    capture exclusion applies to it. A recorder's portal mode completed the ScreenCast handshake
    with the wlr backend (M); frame delivery was not measured.
15. **Weston headless is not usable as the client test bed on its own**: it advertises no
    `wl_seat` (M). Nesting the compositor under test inside it worked and kept everything off the
    live desktop; the same arrangement can host Skorupa's T2 run.
