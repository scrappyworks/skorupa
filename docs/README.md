# Documentation

Start with the design, then the requirements. Every behavior change starts from a `REQ-` id in the catalog and a failing test.

| Document | What it holds | State |
| --- | --- | --- |
| [design.md](design.md) | How Skorupa is meant to work: processes, trust domains, lock, agents, verification, budget | being written |
| [requirements.md](requirements.md) | The requirement catalog: ids, acceptance criteria, proof kinds, stages, open decisions | draft |
| [threat-model.md](threat-model.md) | Assets, principals, attackers, which requirement holds against whom, deployment prerequisites, known gaps | draft |
| [api-map.md](api-map.md) | Decision for every Wayland protocol, portal and D-Bus name, and which domain gets it | being written |
| [ux-niri-parity.md](ux-niri-parity.md) | Detailed UX specification: hot corner, window switchers, overview, gestures, screenshots, recording, and the listed differences from niri | being written |
| [`spec/`](../spec/) | The Quint model of cross-process ordering: lock epochs, presentation acknowledgement, revocation | being written |

Documents marked "being written" are prepared on other branches and may not be present in this tree yet.
