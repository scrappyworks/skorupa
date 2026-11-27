# AGENTS.md

Skorupa is a small Wayland desktop for one laptop screen. Its parts run as separate processes: `compositor/`, `shell/`, `lock/`, and `sandbox/` for AI agents. `protocol/` is the only contract between them.

## Ground rules

- Keep passwords in `lock/`. Only its auth broker unlocks the session, and every error keeps it locked.
- Give clients nothing by default. Every Wayland protocol and IPC call needs a grant, checked on each request. Never treat a UID, PID, app id, or D-Bus name as an identity.
- Draw consent, password, and lock screens in the compositor, above every client.
- Agents get no clipboard, screen capture, or input unless granted for one window and a limited time.
- Support one screen with AMD or Intel graphics on Mesa. Do not add NVIDIA, X11, XWayland, or multi-screen code.
- Prefer the kernel, systemd, logind, and existing daemons over new code. Justify every new protocol, option, and dependency.
- Decide from protocol fields and exit codes, never from human-readable text.
- Write no `unsafe`. Code taken from niri keeps upstream's.
- Put logic in pure `step(state, event) -> (state, effects)` functions, with I/O, time, and randomness in thin adapters. Bound every size, queue, and retry.
- Start each behavior change with a `REQ-` id and a failing test. Keep tests deterministic.
- Never log or store secrets, including in `Debug`, panics, arguments, environment, and snapshots.
