# Skorupa

A small, security-focused Wayland desktop for laptops with one screen.

> **Status:** design phase. Nothing here runs yet.

## Goals

- **Secure by default.** Clients get nothing they were not granted. Password, consent and lock screens cannot be imitated or captured. Screen capture and microphone use are visible and easy to stop.
- **Balanced.** Ask only where the answer changes something, in the style of GNOME and Android.
- **Fast.** Input-to-photon latency and frame pacing at 120 and 180 Hz at least as good as niri.
- **Small.** Under 100k lines of Rust including tests, with no hand-written `unsafe` outside one audited module.
- **Standard.** Wayland, xdg-desktop-portal, freedesktop D-Bus, logind, polkit, PAM and systemd. Applications need no Skorupa-specific code.
- **AI agents in their own desk.** Agents run in a sandbox with a separate headless compositor and never see your windows without a grant.

## Parts

| Directory | Process |
| --- | --- |
| `compositor/` | Wayland compositor on [Smithay](https://github.com/Smithay/smithay) |
| `shell/` | panel, launcher and notifications in [Slint](https://slint.dev) |
| `lock/` | trusted prompt renderer, authentication broker, PAM worker |
| `sandbox/` | launcher and desk for AI agents |
| `protocol/` | the only contract between the processes |

## Scope

One screen. AMD and Intel graphics on Mesa. No NVIDIA, X11, XWayland or multi-monitor support.

## Credits

Parts of the compositor are ported from [niri](https://github.com/YaLTeR/niri) with their authorship kept.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
