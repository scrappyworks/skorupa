# Skorupa in a NixOS VM

The VM boots the Skorupa compositor on one virtio-gpu KMS output in a logind
session. It uses software GLES, opens two xdg-shell test windows as an unprivileged
user, and switches between their workspaces by sending keys to the virtual
keyboard. The test checks the colors on the guest output and verifies that
the selected window receives the typed key. The VM stores its screenshots
in the check output.

Run one VM check at a time. These commands use the default x86-64 guest.

```sh
nix build .#checks.x86_64-linux.vm-session --print-build-logs
```

```sh
nix build .#checks.x86_64-linux.vm-logind --print-build-logs
```

For an interactive console, build
`checks.x86_64-linux.vm-session.driverInteractive` and run
`result/bin/nixos-test-driver`. Use `start_all()` at its prompt to boot the
guest or `test_script()` to launch both windows and exercise the switches.
The normal compositor binary still defaults to headless and opens a DRM
seat only with `--drm`.

## Sleep probes

The logind check uses the guest's real systemd and D-Bus. It replaces the
suspend job with a controlled failure so the test can observe the ordering
without depending on a kernel wakeup. The delay inhibitor survives a
SIGKILL of the user bridge and reaches its replacement as a stored file
descriptor. A second inhibitor requested while preparing for sleep returns
`org.freedesktop.login1.OperationInProgress`. The failed job emits
`PrepareForSleep(false)` with `PreparingForSleep` false, and the preflight
rejects a one-second delay. The guest's test polkit rule grants only its test
user permission to take the delay inhibitor.

The graphical guest exercises ordinary Wayland windows and keyboard input.
The shell, PAM and the trusted lock surface are separate integration work.
Physical output timing and kernel suspend/resume need their own tests.
