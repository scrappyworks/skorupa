# Requirements

This is the requirement catalog for Skorupa. Every behavior change starts from an id in this file and a failing test whose name contains that id.

- **Status:** pre-implementation draft, 2026-11-27. Apart from the first `protocol/` tests, no requirement has a passing proof yet.
- **Precedence:** the design document ([design.md](design.md)), where its section "Wynik dyskusji modeli" (outcome of the model discussion) overrides the rest of its text; then the owner decisions recorded after that discussion; then the six two-round reviews.
- **Related:** [threat-model.md](threat-model.md) says which requirement holds against which attacker. [api-map.md](api-map.md) holds the per-protocol decisions. [ux-niri-parity.md](ux-niri-parity.md) holds the detailed UX specification. `spec/` holds the Quint model.

## How to read the tables

| Column | Meaning |
| --- | --- |
| **Id** | `REQ-<AREA>-<NNN>`. Ids are stable: a retired requirement keeps its number and is marked retired, a new one takes the next free number. |
| **Requirement** | One testable sentence with "must". |
| **Why** | One-line rationale. |
| **Accepted when** | The observable criterion. |
| **Proof** | The kind of evidence required (legend below). The first kind listed is the primary one. |
| **Stage** | `v1` or `later`. `bring-up` marks a rule that applies only to the first hardware milestone with the unchanged Rygiel lock. |
| **Src** | Where the requirement comes from (legend below). |

**Proof kinds:**

| Kind | Meaning |
| --- | --- |
| `unit` | Deterministic test of a pure reducer or a bounded adapter, with events and time passed in. |
| `prop` | Property test over generated operation sequences, with seed and shrinking. |
| `client` | Headless compositor test with real Wayland clients over a socket pair. |
| `sim` | Simulation that composes the lock, grant and `authd` reducers with injected faults, driven by the fuzzer. A failure returns seed and trace. |
| `fuzz` | Fuzz target for a decoder or parser. |
| `quint` | Invariant checked in the single Quint model of cross-process ordering; counterexamples are replayed against the Rust reducers. |
| `kani` | Bounded Kani proof of a small function. |
| `vm` | NixOS VM test with real processes (virtio-gpu or VKMS, real logind and PAM). |
| `hw` | Test on a target laptop on the test rig (REQ-OPS-012). |
| `gate` | Scripted check in the local gate: lint, line count, linked-crate list, configuration snapshot. |
| `review` | Checked by a reviewer against a written checklist; used for prose, policy and configuration. |

**Source codes:** `W` discussion outcome section of the design document; `D` rest of the design document; `B` owner decisions in the preparation brief; `R1`, `R2` review rounds; `G` ground rules in `AGENTS.md`; `RS-nn` research file `nn`; `M` findings of the Quint model (`spec/README.md`); `P` measurements of the API probe ([api-map.md](api-map.md)); `X` cross-review of the preparation branches.

**Markers:** `[open: O-n]` means the requirement depends on open decision `O-n` ([list](#open-decisions)); the sentence states only what is already agreed. `[proposed]` means one reviewer proposed it and nobody objected, but the owner has not confirmed it.

## Core properties

The four properties every release must show evidence for. Each is a requirement in its area table.

| Property | Statement | Id |
| --- | --- | --- |
| **A** | The session never unlocks without an authentication in the current lock epoch. | REQ-AUTH-001 |
| **B** | After the lock is acknowledged, no frame of the unlocked session reaches the screen. | REQ-LOCK-001 |
| **C** | No grant survives its revocation. | REQ-GRANT-001 |
| **D** | An agent never gets capture or input without a grant. | REQ-AGENT-001 |

Release evidence for A and B is REQ-OPS-014. Property B has a stated limit around sleep that Skorupa does not control: see REQ-LOCK-012, REQ-LOCK-018 and open decision O-29.

## Lock failure table

Every row must end in a locked session and none in an unlocked one. Two rows state a limit: what can be visible on wake after a sleep that Skorupa did not control.

| Failure | Required behavior | Id |
| --- | --- | --- |
| No active output (lid closed, panel off, no connector) | The lock is acknowledged on confirmed output-off with input and scene already in lock mode; the first frame on any output that becomes active is a lock frame. | REQ-LOCK-002, REQ-LOCK-010 |
| Failed render or page flip of the lock frame | The lock is never cancelled; the compositor retries and does not acknowledge until a lock frame is presented or the output is off. | REQ-LOCK-011 |
| Sleep begins without a lock acknowledgement (logind's delay ran out, or the inhibitor was lost) | Skorupa cannot prevent this sleep. On wake the compositor locks before it handles anything else. The frame shown before sleep may stay visible until the first lock frame is presented. | REQ-LOCK-012 |
| A valid verdict arrives after the inhibitor is released and before the machine suspends | Not closed yet: the machine sleeps showing the session and locks again on wake. The proposed sleep-hold rule is open decision O-29. | REQ-LOCK-018 |
| Compositor crash or restart | The session ends and comes back only through an authenticating greeter. | REQ-LOCK-013 |
| `lock/ui` crash or crash loop | A compositor-owned fallback surface stays up; the session stays locked; the UI restarts with a bounded backoff. | REQ-LOCK-014 |
| First frame after resume | The first frame submitted after a resume is a lock frame; a flip left over from before the sleep never counts as the lock being shown. | REQ-LOCK-015 |
| `CLOCK_BOOTTIME` jump | A jump against `CLOCK_MONOTONIC` means the machine slept; the check runs before every event, and the next frame is a lock frame. | REQ-LOCK-016, REQ-LOCK-025 |
| logind `Unlock` signal | Ignored. `Lock` is honored. | REQ-LOCK-017 |

## LOCK: session lock in the compositor

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-LOCK-001 | **Property B.** After the compositor acknowledges a lock, it must present no frame containing unlocked session content until an authorized unlock (REQ-AUTH-001). | The acknowledgement is what sleep and the user rely on. | Normal and protected test content are distinguishable in the output; no normal-content frame is observed between acknowledgement and unlock in any injected fault order; KMS frame CRC confirms it where the driver allows. | sim, quint, vm, hw | v1 | D, W |
| REQ-LOCK-002 | The compositor must acknowledge a lock for exactly two causes: the page flip that carried a lock frame has completed on the active output, or the output is confirmed off while input is in lock mode and the scene is the lock scene. | A queued frame can fail, a session frame queued before the request can complete after it, and a closed lid gives no flip at all. | Frames are tagged when submitted and the tag is read back at the flip event; the flip of an untagged frame during Locking yields no acknowledgement; no third path to the acknowledgement exists. | unit, quint, vm | v1 | B, R2, M, X |
| REQ-LOCK-003 | A lock request must switch input routing to lock mode in the same reducer step, before any later input event reaches a client. | Keystrokes typed after the request must not reach applications. | Any input event ordered after the lock request is delivered to no client surface. | unit, prop | v1 | D |
| REQ-LOCK-004 | The compositor must honor a lock request from logind `Lock`, the lock shortcut, the idle timer and sleep preparation, and must accept no request that cancels a lock in progress. | Lock is compositor state, not an add-on client. | Each source leads to Locked; no event other than an `authd` verdict leaves Locking or Locked. | unit, prop, quint | v1 | D, W |
| REQ-LOCK-005 | The idle-lock timer must run in the compositor. | A hung shell must not disable auto-lock. | With the shell stopped, idle time still locks the session. | unit, vm | v1 | W |
| REQ-LOCK-006 | The compositor must honor an idle inhibitor only while its surface is visible. `[proposed]` | A hidden client could block auto-lock forever. | An inhibitor on a surface that is not visible does not delay the idle lock. | unit, client | v1 | R1 |
| REQ-LOCK-007 | The compositor must mint a new lock epoch for every lock and never reuse an epoch within one compositor instance. | Verdicts are bound to the epoch (REQ-AUTH-001). | Epochs are strictly increasing; a verdict for an earlier epoch is rejected. | unit, kani | v1 | W, R2 |
| REQ-LOCK-008 | The lock UI connection must carry no object or message that can request an unlock. | Under `ext-session-lock` the lock client itself requests unlock; Smithay marks the session unlocked before calling the compositor. | The role given to `lock/ui` has no unlock request in its protocol; a client that sends one is disconnected and the session stays locked. | client, review | v1 | W, B |
| REQ-LOCK-009 | The `ext-session-lock` global must exist only in the bring-up build, and even there `unlock_and_destroy` alone must not be the unlock authority once `authd` verdicts are wired. | Needed to run the unchanged Rygiel in the first hardware milestone only. | The v1 build does not advertise the global to any domain; a build flag test fails if it does. | client, review | bring-up | W, B |
| REQ-LOCK-010 | With the output off, the compositor must acknowledge by the second cause of REQ-LOCK-002, and the first frame it submits when an output becomes active must be a lock frame. | A closed lid gives no page flip to wait for. | Lock with the output off acknowledges without a flip, and only after input and scene are switched; re-enabling the output shows the lock frame first. | unit, sim, vm, hw | v1 | W, R1, X |
| REQ-LOCK-011 | A failed render or page flip of a lock frame must never unlock the session or cancel the lock. | niri unlocks when the locked frame fails to render. | Injected render and flip failures during Locking keep input in lock mode, emit no acknowledgement and no unlock, and are retried. | unit, sim, vm | v1 | W |
| REQ-LOCK-012 | When sleep begins without a lock acknowledgement, because logind's delay ran out or the last inhibitor descriptor was lost, the compositor must lock before it handles any other event once it runs again, and must submit no session frame until an authorized unlock. `[open: O-4]` | Skorupa cannot stop this sleep: logind bounds the delay, and the inhibitor ends with the process that holds it. | With flips blocked past the delay, and separately with the inhibitor holder killed, the machine sleeps and wakes locked. What may be visible on wake is the last frame shown before sleep, which can be a session frame, until the first lock frame is presented; nothing newer is shown and no input reaches a client. | sim, quint, vm, hw | v1 | W, R2, M, X |
| REQ-LOCK-013 | When the compositor exits for any reason, the session must end, and a new session must start only through a greeter that authenticates. | A restarted compositor has no trustworthy lock state. | Killing the compositor while locked or unlocked returns to the greeter; no configuration path yields an unlocked session without authentication. | vm, review | v1 | W |
| REQ-LOCK-014 | Whenever the session is not unlocked and `lock/ui` has no surface (not started yet, exited, or crash-looping), the compositor must show its own cover, stay locked, and restart the UI with a bounded backoff. `[open: O-6]` | The session must never fall back to unlocked content, and the start time of the UI must not count against logind's sleep delay. | The lock is acknowledged on the cover alone; killing the UI repeatedly never shows unlocked content; restart attempts are rate-limited; unlock works again once a UI survives. | sim, quint, vm | v1 | D, W, M |
| REQ-LOCK-015 | After a resume, the first frame the compositor submits must be a lock frame, and a flip completion for a frame submitted before the sleep must never count as presentation of the lock. | The stale pre-sleep frame is the classic leak, and a pre-sleep flip can complete after wake. | After a timed wake, the first submitted frame is a lock frame and the acknowledgement follows only that frame's flip; the time from wake to the presented lock frame is measured and recorded per laptop. | quint, vm, hw | v1 | W, X |
| REQ-LOCK-016 | When `CLOCK_BOOTTIME` advances past `CLOCK_MONOTONIC` by more than the configured threshold, the compositor must lock and make the next frame a lock frame. `[open: O-5]` | It catches every sleep, including one the logind path missed. | A simulated clock jump with no `PrepareForSleep` leads to Locked before any normal frame (see REQ-LOCK-025 for when the check runs). | unit, sim, quint | v1 | W, M |
| REQ-LOCK-017 | The compositor must ignore the logind `Unlock` signal and honor the logind `Lock` signal. | `loginctl unlock-session` must not bypass authentication. | `Unlock` in every state changes nothing; `Lock` locks. | unit, vm | v1 | W, R1 |
| REQ-LOCK-018 | While Skorupa holds the sleep delay inhibitor, it must release it only on a lock acknowledgement that answers the current sleep request (REQ-LOCK-024). `[open: O-29]` `[open: O-30]` | This is the part of sleep that Skorupa controls. | At the moment of release the panel shows a lock frame or is off (REQ-LOCK-002). Not guaranteed without the rule of O-29: that the session is still locked when the machine suspends, because a valid verdict can arrive between release and suspend. A missing acknowledgement leaves the lock request pending for retry. | unit, quint, vm | v1 | D, M, X |
| REQ-LOCK-019 | While locked, the compositor must render and give input to no client surface except the lock UI and its own fallback, and must refuse session-exit actions. | Unlocked windows must be neither visible nor reachable. | No other surface is in the render list or receives events; the exit action does nothing. `[open: O-25]` for which hardware keys stay active. | prop, client, vm | v1 | D, R1 |
| REQ-LOCK-020 | On lock, the compositor must clear its retained clipboard copy and the primary selection. | The copy outlives the source app (REQ-API-006) and must not outlive the lock. | After lock, the retained copy is empty and no client can read the previous selection. | unit, client | v1 | W |
| REQ-LOCK-021 | On lock, every live capture stream and every agent grant must end (REQ-GRANT-006). | A stream must not see the lock screen or run unattended. | No frame is delivered to any stream after lock; grants need a new decision after unlock. | sim, vm | v1 | D, W |
| REQ-LOCK-022 | The lock screen must show no notification content. | Message text is private. | With a notification arriving while locked, no body or summary is rendered. | unit, vm | v1 | W |
| REQ-LOCK-023 | Input devices attached while locked must work for authentication. | The Z13 keyboard is detachable. | Attaching the keyboard while locked allows typing the password; detaching leaves the touch path working (REQ-TRUST-008). | vm, hw | v1 | R1 |
| REQ-LOCK-024 | A lock acknowledgement to the bridge must echo the nonce of the lock request it answers and must be sent only while Locked, and the bridge must ignore an acknowledgement with any other nonce. | logind's `PrepareForSleep` carries no cycle id; a stale acknowledgement would release the next sleep while the session is unlocked. | The acknowledgement of an earlier sleep cycle never releases the inhibitor of a later one. | unit, quint | v1 | M |
| REQ-LOCK-025 | The compositor must compare `CLOCK_BOOTTIME` with `CLOCK_MONOTONIC` before it handles any event, including a verdict, and every reducer event must carry both clock readings. | A check made only before drawing is too late: a verdict for a secret typed before the lid closed can arrive after it opens. | A verdict delivered as the first event after a simulated jump is rejected and the session stays locked. | unit, sim, quint | v1 | M |

## AUTH: authentication broker, PAM worker, bridge

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-AUTH-001 | **Property A.** The compositor must leave Locked only by consuming a one-use `authd` verdict bound to the compositor instance, the session, the current lock epoch and the attempt. | A UI or bus message must never be enough to unlock. | A verdict with any mismatched field, a replayed verdict and a verdict received outside Locked are all rejected and leave the session locked. | unit, kani, sim, quint | v1 | W, B |
| REQ-AUTH-002 | Only `authd` must be able to issue a verdict, over a connection it received by descriptor inheritance. | Identity by name, PID or UID can be forged. | The verdict message exists only on the `authd` channel type; no other role's decoder accepts it. | unit, review | v1 | W, G |
| REQ-AUTH-003 | Every authentication error must leave the session locked. | Fail closed. | Timeout, disconnect, malformed frame, worker unavailable, wrong epoch and PAM failure each end in Locked. | prop, sim | v1 | D, G |
| REQ-AUTH-004 | A verdict must be unusable after `authd` disconnects, after a re-lock, or after the attempt it answers is superseded. | Old successes must not unlock a newer lock. | Disconnect the broker, lock again, then deliver the old verdict: rejected. | sim, vm | v1 | D |
| REQ-AUTH-005 | `pwcheck` must run `pam_authenticate` and `pam_acct_mgmt` and must accept only when the PAM user equals the requested user. | Dropping the account check would unlock expired or locked accounts. | An expired account with a correct password is rejected. | vm | v1 | W |
| REQ-AUTH-006 | `pwcheck` must run as root without capabilities, be socket-activated, serve one request per process and keep no state between requests. | Smallest privileged surface. | The unit file and process inspection show no capabilities; a second request on one process is refused. | vm, review | v1 | D |
| REQ-AUTH-007 | A password must travel only from the credential instance of `lock/ui` through `authd` to `pwcheck`, in a bounded binary frame of at most 512 bytes that is overwritten after use. `[open: O-7]` | One short, auditable path for the secret. | Oversized frames are rejected before any work; memory holding the secret is zeroed after the reply. | unit, fuzz, review | v1 | D |
| REQ-AUTH-008 | No process must log or store a secret, including in `Debug` output, panic messages, arguments, environment, snapshots and core dumps. | Logs and dumps outlive the session. | Secret types have no `Debug` that prints content; a test greps logs and test snapshots for a canary password. | unit, vm, review | v1 | G |
| REQ-AUTH-009 | The compositor must never store or log key events routed to a credential prompt. | The compositor does handle these keystrokes, so "never sees passwords" is false; the rule is "never keeps them". | No buffer, trace or log in the compositor contains the canary password after an unlock. | unit, review | v1 | R2 |
| REQ-AUTH-010 | `authd` must have no GUI, no network access and no D-Bus library. | It is the reused, proven reducer; it must stay small. | The linked crate list has no D-Bus or network crate; the unit runs with a private network. | review, vm | v1 | W |
| REQ-AUTH-011 | The D-Bus bridge for logind and polkit must live in `lock/bridge` and must hold no unlock authority. `[open: O-10]` | D-Bus is a large parser; it must not sit next to the unlock decision. | No message type from the bridge leads to Unlocked in the reducers. | unit, quint | v1 | W |
| REQ-AUTH-012 | A polkit authentication must complete through polkit's own helper, and the bridge must never hold the password. `[open: O-9]` | polkit binds the result to its cookie; a custom path would bypass it. | A polkit action succeeds only through the helper; the bridge process never receives secret bytes. | vm | v1 | W |
| REQ-AUTH-013 | Reducers reused from Rygiel must stay unchanged, with their Lean proofs frozen and a drift check that fails when the reducer source changes. | The proofs hold only for the code they were extracted from. | Editing a covered reducer without updating the proof fails the gate. | review | v1 | B, R2 |
| REQ-AUTH-014 | The unlock decision must not depend on wall-clock time or on any human-readable text. | Clock changes and localized messages must not change the verdict. | Reducers take time as data; adapters decide from PAM return codes only. | unit, review | v1 | G |
| REQ-AUTH-015 | The compositor instance id bound into verdicts and consent prompts must be a random value chosen at compositor start, never a counter or a PID. | Epoch and attempt counters restart with the compositor, so the instance id is the only field that tells an old verdict from a new attempt. | A verdict recorded for one instance is rejected by the next instance with equal epoch and attempt numbers; the id comes from the system random source. | unit, quint, review | v1 | M |
| REQ-AUTH-016 | A resume must close the open unlock attempt, and a verdict for an attempt opened before the last resume must be rejected. | Otherwise a secret typed before the lid closed unlocks for whoever opens it. | Type the secret, sleep before the verdict arrives, wake: the late verdict is rejected. | unit, sim, quint | v1 | M |
| REQ-AUTH-017 | When the `authd` connection closes, the compositor must close the open attempt at once and reject every verdict issued for it, and a restarted `authd` must send no verdict for a challenge it did not consume. | The model accepted a queued success after a broker restart; REQ-AUTH-004 must hold without a re-lock in between. | Disconnect the broker with a positive verdict in flight and do not lock again: the verdict is rejected and the session stays locked. | unit, sim, quint | v1 | M, X |
| REQ-AUTH-018 | Each unlock attempt must use a new credential channel that the challenge names, and `authd` must take one credential per challenge, from that channel only. | With one long-lived channel, a secret typed for an attempt that timed out is judged for the next one. | A credential written to the channel of a closed attempt is never read for a later attempt. | unit, quint | v1 | M |

## TRUST: trusted UI for credentials, consent and lock

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-TRUST-001 | Credential, consent and lock prompts must be drawn by `lock/ui` with its own rasterizer and composited above every client surface. | A small renderer without Slint keeps the trusted base small. | No client surface, including fullscreen and layer-shell ones, is ever stacked above a trusted prompt. | prop, client | v1 | W, B |
| REQ-TRUST-002 | Credential prompts and consent prompts must run in separate instances, each in a fresh process per prompt. | The consent instance handles untrusted strings (titles, app names) and must have no password channel. | The consent instance has no descriptor to `authd`; each prompt has a new PID. | vm, review | v1 | W, B |
| REQ-TRUST-003 | The compositor must own a reserved strip that no client can draw into, and must show the trusted-mode mark there. `[open: O-1]` | A fullscreen client owns every other pixel, so only reserved pixels are trustworthy. | In trusted mode, no client buffer covers the strip, including fullscreen clients. | prop, client, hw | v1 | W, B |
| REQ-TRUST-004 | A physical reveal gesture, handled by the compositor before any client, must show the true domain of every window and the active grants, including over fullscreen. `[open: O-3]` | The user needs a check that a client cannot fake. | The gesture is never delivered to a client; the reveal shows compositor-held state only. | client, hw | v1 | W, D |
| REQ-TRUST-005 | Only devices that pass the device-provenance rule must be able to trigger the reveal gesture, the kill switch, or confirm a trusted prompt. `[open: O-2]` | `uinput` creates virtual keyboards below Wayland. | Events from a virtual device are rejected for these three uses. | unit, vm | v1 | W |
| REQ-TRUST-006 | In trusted mode, the compositor must give input only to the active trusted instance and must disable overlays, capture, clipboard access, input methods and synthetic input. | Otherwise a client could read, cover or drive the prompt. | During a prompt, no other client receives input events, clipboard offers or frames. | prop, client, sim | v1 | D, W |
| REQ-TRUST-007 | Trusted surfaces must be excluded from every capture path. | A stream must not record a password prompt. | Captured frames taken during a prompt contain no pixel of it. | client, vm | v1 | D, B |
| REQ-TRUST-008 | Every trusted prompt must be completable by touch alone, with an on-screen keyboard inside the trusted UI. | The Z13 works without its keyboard, and trusted mode disables the shell's input method. | Unlock and consent succeed in a test with touch events only. | vm, hw | v1 | W, R1 |
| REQ-TRUST-009 | While a trusted mark or prompt must be visible, the compositor must compose the frame itself and not scan out a client buffer directly. | Direct scanout would hide the reserved pixels. | Entering trusted mode over a scanned-out fullscreen client shows the strip on the next presented frame. | unit, hw | v1 | R1 |
| REQ-TRUST-010 | When a prompt process exits without an answer, the request must be denied, and normal input must return only if the session is unlocked; while locking or locked, input must stay in lock mode with the cover visible (REQ-LOCK-014). | Fail closed; never leave input captured in an unlocked session, and never release it in a locked one. | Killing a consent prompt in an unlocked session yields a denial and normal input within one frame; killing the credential UI while locked delivers no input to any client and shows the cover. | unit, sim, quint | v1 | G, X |
| REQ-TRUST-011 | A consent prompt must name the requester from compositor-held identity, and must say "unknown application" when there is none. | Client-supplied names are a spoofing channel. | A client that sets a misleading title or app id cannot change the requester line. | unit, client | v1 | W, D |
| REQ-TRUST-012 | At most one trusted prompt must be active at a time, and pending consent requests must wait in a bounded queue. `[open: O-28]` | Prompt floods are a denial of service and a fatigue attack. | Requests beyond the queue bound are denied; two prompts are never on screen together. | unit, prop | v1 | G, R1 |
| REQ-TRUST-013 | A consent prompt must ignore input for a short delay after it appears, and Enter must never mean Allow. `[proposed]` | It stops click-through and type-ahead approval. | Input within the delay is discarded; no keyboard default approves. | unit | v1 | R1 |
| REQ-TRUST-014 | The consent instance must render any untrusted string without failing, with a visible replacement for glyphs it cannot draw. `[open: O-27]` | Rygiel's renderer has a Latin-subset font; window titles are arbitrary. | A corpus of mixed-script, emoji and control-character titles renders without crash or layout escape. | fuzz, unit | v1 | R1 |

## GRANT: default deny, grants and revocation

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-GRANT-001 | **Property C.** After a revocation is processed, the compositor must deliver no further frame, input event or clipboard byte under that grant. | A revocation that leaks one more frame is not a revocation. | In every interleaving of use and revoke, nothing is delivered after the revoke step. | prop, sim, quint, kani | v1 | D |
| REQ-GRANT-002 | A client must get no privileged Wayland global or IPC call by default, and the compositor must check the grant when the object is created, on every sensitive request, and after revocation. | A bind-time check alone goes stale. | A client that keeps an object after losing its grant gets an error on the next request. | client, prop | v1 | D, G |
| REQ-GRANT-003 | The compositor must derive a client's domain only from how the connection was made, never from UID, PID, app id or a D-Bus name. | Those values are forgeable or shared. | Domain assignment code takes only the listener or inherited descriptor as input. | unit, review | v1 | D, G |
| REQ-GRANT-004 | A connection without domain metadata must get the lowest domain. | Missing information must not raise privilege. | A plain `wayland-0` connection sees only the base globals. | client | v1 | D |
| REQ-GRANT-005 | A revocation must close the stream, drop queued input and cancel a pending clipboard transfer in the same reducer step. | No window between "revoked" and "stopped". | One revoke event yields all three effects before any other event is handled. | unit, quint | v1 | D |
| REQ-GRANT-006 | Every live grant must end at lock, at suspend, at compositor restart, when its target window goes away, when it expires, and when its broker fails. | Live access must not continue unattended. | Each of the six events ends a live stream in a test. | sim, vm | v1 | D, W |
| REQ-GRANT-007 | A grant or share on a window must be bound to the compositor's window identity and generation, and must be rejected when the window has been replaced. | A stale id must not point at a new window. | A grant or window share used after the window is destroyed and its id reused is rejected. | unit, prop | v1 | D |
| REQ-GRANT-008 | "Stop" must address one stream generation of one holder, never a reusable identifier or the holder alone. | Identifiers can be recycled (stated by the portal documentation per R2; not re-checked), and a holder can have a newer stream. | Stop for an ended generation never stops a newer stream of the same holder or one that reuses the id. | unit, quint | v1 | W, R2, M, X |
| REQ-GRANT-009 | A connection created through a security context must never get more than the `sandboxed` domain, and only contexts created on the launcher's connection must get the `agent` domain; nested contexts must be refused. `[open: O-11]` | The label is supplied by the sandbox engine; nesting would let a sandbox pick its own label. | A sandboxed client cannot create a context; an `agent` label from a non-launcher connection is refused. | client | v1 | D, R1 |
| REQ-GRANT-010 | The compositor must not implement `wlr-screencopy`, `data-control`, `gamma-control`, `output-management` or `export-dmabuf`. | Each is an ambient capture or control path. | `wayland-info` on every domain's connection lists none of them. | client | v1 | D |
| REQ-GRANT-011 | The `skorupa` CLI and any same-user socket must never grant a permission, unlock the session or capture the screen. | Any process of the user can call them. | The CLI protocol has no such message; a fuzzed CLI socket cannot reach those effects. | unit, fuzz | v1 | R1 |
| REQ-GRANT-012 | Each domain must get exactly the globals listed for it in [api-map.md](api-map.md). | The matrix is the policy; drift is a hole. | A snapshot test of advertised globals per domain matches the map. | client | v1 | D |
| REQ-GRANT-013 | The adapter that executes a delivery must check the grant at the last point Skorupa controls, immediately before the frame, event or data leaves: the connection, the stream or window generation, the deadline against the clock at that moment, and the lock state; it must drop the delivery otherwise. | An effect emitted before a lock, a reconnect or a deadline is executed after it; clearing queues in the reducer cannot stop what already left it. | A delivery effect emitted before each of the four changes and executed after it delivers nothing. | unit, sim, quint | v1 | M, X |
| REQ-GRANT-014 | A grant must belong to one connection, and a holder that reconnects must have none. | A name or app id that survives a reconnect is not an identity. | After the holder's connection closes, a new connection with the same label receives nothing on the old grant. | unit, quint | v1 | M |
| REQ-GRANT-015 | A consent answer must create a grant only for the open prompt of the current compositor instance, only while unlocked, and must consume the prompt. | A stale or replayed answer must not create a grant. | An answer for a closed prompt, for another instance, or arriving while locked creates nothing. | unit, quint | v1 | M |

## CAP: screen capture, screenshots, recording

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-CAP-001 | An application must get a live stream of screen or window content only through the ScreenCast portal, after the user picks the source in the compositor's chooser, with a fresh pick for every share. | This is the stream path; still images are REQ-CAP-007. A portal means nothing if restore or a direct protocol bypasses it. | A share with valid restore data from an earlier session still shows the chooser; no other interface delivers a stream. | vm | v1 | W, B, X |
| REQ-CAP-002 | The portal backend's connection must be able to capture only the picked source, and the compositor must refuse every other source. | xdg-desktop-portal-wlr must never be the enforcement point. | A modified backend that asks for another window or output gets an error. | client, vm | v1 | R2 |
| REQ-CAP-003 | A share must be bound to the requesting application, the picked source and the stream, and the chooser must show the requester (REQ-TRUST-011). | Stock xdg-desktop-portal-wlr gives the chooser no requester identity. | Two concurrent requests cannot swap sources; the chooser names the right requester. | vm | v1 | W, B |
| REQ-CAP-004 | Captured frames must contain no pixel of trusted surfaces, the lock screen or the notification layer. | Private content must not leak into a call. | A capture taken while a notification and a prompt are visible shows neither. | client, vm | v1 | W |
| REQ-CAP-005 | A cancelled or failed chooser must decline the request with no source. | No silent fallback to "whole screen". | Killing the chooser process or returning empty output yields a declined request. | vm | v1 | R2 |
| REQ-CAP-006 | An indicator must be visible while any capture is live, and its Stop must end delivery within a bounded time. | The user must see and be able to end sharing. | Stop ends frame delivery within the bound; the indicator is never absent during a live stream. | client, vm | v1 | D, W |
| REQ-CAP-007 | An application must get a still image of the screen only through the Screenshot portal, served by Skorupa's own adapter, which must write only to a newly created file that other users cannot read and never to a fixed path. `[open: O-19]` | This is the still-image path, separate from streams. xdg-desktop-portal-wlr runs `grim` and writes `/tmp/out.png`. | Two screenshots never share a path; the file is created exclusively with owner-only mode; REQ-CAP-004 holds for the image; no other interface delivers a still image. | unit, vm | v1 | W, B, X |
| REQ-CAP-008 | Titles and app ids of windows must not be written to logs at the default log level by any capture component. | xdg-desktop-portal-wlr logs every capturable window title at INFO before the chooser runs. | After a cancelled share with a canary window title, the journal does not contain the canary. | vm | v1 | W |
| REQ-CAP-009 | Screen sharing of a selected window must work from current Firefox and Chromium. | This is the main use (calls). | A Meet test call on the Z13 shares exactly the picked window, and Stop ends it. | hw | v1 | D |
| REQ-CAP-010 | The user's own screenshot and recording actions must work without giving any client an ambient capture right. `[open: O-18]` `[open: O-19]` | Parity (REQ-UX-006, REQ-UX-007) without reopening the hole. | Screenshot and recording work with only the globals of REQ-GRANT-012. | client, vm | v1 | B, R1 |
| REQ-CAP-011 | An agent must be able to capture one user window once, with a preview shown to the user, bound to window identity and generation. | Useful, but deliberately discloses user data; needs its own revocation tests. | The capture contains only that window; a replaced window is refused. | client, sim | later | W, B |
| REQ-CAP-012 | When the user picks a region, the crop must be applied before any recorder or requester receives a buffer. `[open: O-18]` | A rectangle drawn in a chooser does not constrain the pixels that are delivered; the stock portal backend accepts only a monitor or a window. | A consumer of a region capture never receives a pixel outside the region. | client, vm | v1 | X |

## AGENT: AI agents on their own desk

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-AGENT-001 | **Property D.** An agent must never receive a frame of user content or deliver an input event to a user window without a live grant; in v1 no such grant exists. | The sandbox is the only strong boundary and must not be pierced silently. | No effect in the reducers delivers a host frame or host input to an agent connection without a grant record. | prop, sim, quint, vm | v1 | D, W |
| REQ-AGENT-002 | Each agent must run on its own headless compositor instance inside its sandbox, with a private socket, seat and clipboard, rendered in software with Pixman. | No shared compositor state to get wrong. | The agent's Wayland socket is not the host's; the instance holds no DRM or host input descriptor. | vm, review | v1 | W, B |
| REQ-AGENT-003 | When the restricted environment for an agent cannot be set up, the agent must not start. | No fallback to an unrestricted connection. | Each injected setup failure ends with no agent process. | unit, vm | v1 | D |
| REQ-AGENT-004 | The host compositor must offer no virtual keyboard or virtual pointer to any client in v1. | These protocols target a whole seat, not a window. | The globals are absent on the host for every domain. | client | v1 | W, B |
| REQ-AGENT-005 | Clipboard data must move between the user and an agent only as one explicit, user-started transfer of one bounded item to one named target. | No ambient clipboard for agents; no secrets filter exists in v1. | Without the action, neither side can read the other's clipboard; oversize items are refused. | unit, client, vm | v1 | W, B |
| REQ-AGENT-006 | The kill switch must first invalidate all agent grants and then freeze the agent cgroups, in that order. | Freezing first would leave live grants on frozen clients. | The effect order is fixed; after the switch, no agent process runs and no grant is live. | unit, quint, vm | v1 | D, R2 |
| REQ-AGENT-007 | A compositor-owned indicator must show each agent that holds a live grant, with its target and rights. `[open: O-21]` `[open: O-26]` | The user must see what an agent can reach. | The indicator matches the grant table in every state. | prop, client | later | D |
| REQ-AGENT-008 | An agent must have no path to answer a trusted prompt. | Consent given by the requester is no consent. | Follows from REQ-AGENT-004, REQ-TRUST-005 and REQ-TRUST-006; an agent-driven attempt in the boundary test fails. | vm | v1 | D |
| REQ-AGENT-009 | The agent boundary must have a test for each of: global shortcuts, direct access to host services, stale window ids, input queued at revocation, protected frames in capture, and prompt injection shown in a visible window. | These are the known ways the boundary breaks. | All six tests exist and pass; the prompt-injection case shows that screen content cannot widen a grant or the egress policy. | vm, sim | v1 | D, RS-01 |
| REQ-AGENT-010 | An agent must not be able to deliver input to a user window. | An input grant on a terminal or browser is a sandbox escape by design. | No protocol or IPC message carries agent input to the host seat. | client, review | v1 | W, B |

## SBX: sandbox for agents and providers

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-SBX-001 | An agent sandbox must contain no host session socket: no host Wayland socket, session D-Bus, accessibility bus, PipeWire socket or compositor IPC socket. | A protocol allow-list is useless if the same power is reachable over another socket. | A probe inside the sandbox finds none of them. | vm | v1 | D, RS-01 |
| REQ-SBX-002 | An agent sandbox must contain no `/dev/input`, no `/dev/uinput`, and no GPU device node unless the profile asks for one. | Raw devices bypass the compositor. | A probe inside the sandbox cannot open them. | vm | v1 | W, R2 |
| REQ-SBX-003 | Outbound network traffic from an agent must pass an egress gate with an allow-list of hosts, denying by default. | Exfiltration control must sit outside the model. | A connection to a host not on the list fails. | vm | v1 | D |
| REQ-SBX-004 | Each agent must run in its own cgroup that the launcher can freeze and kill as a whole. | The kill switch needs one handle on the process tree. | Freezing stops every process of the agent, including children. | vm | v1 | D |
| REQ-SBX-005 | Each agent session must start from a fresh profile with new namespaces. | No state leaks between tasks. | Files written in one session are absent in the next. | vm | v1 | D |
| REQ-SBX-006 | The sandbox must not rely on a Wayland label for isolation from the operating system. | A label classifies a connection; it confines nothing. | The isolation tests of REQ-SBX-001 and REQ-SBX-002 pass with the compositor absent. | vm, review | v1 | R2 |
| REQ-SBX-007 | Untrusted images must be decoded in a separate process with size and time limits and an explicit format list. | Decoders are a common memory-safety target. | An oversized or slow image is rejected within the limits without affecting the parent. | unit, fuzz | v1 | D |

## PERM: permissions the user sees

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-PERM-001 | The system must track and show three separate states: live streams, remembered decisions, and observed activity. | Mixing them makes Stop and Forget ambiguous. | The model has three collections; each UI row belongs to exactly one. | unit | v1 | W, B |
| REQ-PERM-002 | One panel popup must list microphone, camera and screen use separately, each row with application, source and Stop where Stop is enforceable. | One place to look during a call. | With all three active, three rows appear with the right source. | unit, vm | v1 | D |
| REQ-PERM-003 | Stop must act at the last point Skorupa controls, and the UI must not offer Stop where it controls none. | A button that does nothing is theater. | For an unsandboxed microphone user, the row shows activity without a per-app Stop. | unit, review | v1 | W |
| REQ-PERM-004 | Each remembered decision must have Forget, and Forget must also end the matching live stream. | Revoking a decision while the stream runs on is a trap. | Forget during a live camera stream ends the stream (unverified for the installed PipeWire stack; needs the runtime test). | vm | v1 | D, RS-11 |
| REQ-PERM-005 | The system must show "unknown application" when it cannot attribute a stream. | A wrong name is worse than none. | An unattributed stream is listed as unknown, never guessed. | unit | v1 | D |
| REQ-PERM-006 | v1 must not prompt per application for the microphone and must not filter secrets in the clipboard. | Neither can be enforced for unsandboxed apps; no general microphone portal exists. | No such prompt or filter exists in v1. | review | v1 | W |
| REQ-PERM-007 | Remembered decisions must survive lock and restart; live grants must not (REQ-GRANT-006). | Balanced like GNOME and Android: ask once, re-consent for live access. | After restart, a remembered camera decision still applies and no stream is live. | unit, vm | v1 | W, B |
| REQ-PERM-008 | `skorupa permissions` must list and revoke decisions, with stable exit codes and `--json`. | There is no settings GUI. | Listing and revoking work from the CLI and are covered by tests on exit codes. | unit | v1 | D |
| REQ-PERM-009 | The indicators must distinguish a muted device, an available camera and an active recording. | These are three different facts. | Each state has a distinct, tested presentation. | unit | v1 | D |
| REQ-PERM-010 | Sandboxed applications must reach the camera only through the Camera portal, with the decision remembered. | It is the enforceable path. | A sandboxed client without a decision gets a prompt; with one, access without a prompt. | vm | v1 | D |
| REQ-PERM-011 | Camera consent must be asked by a Skorupa backend for the portal's `Access` interface that draws the prompt in the trusted UI. `[open: O-31]` | With the stock backend the camera prompt is an ordinary toolkit window that names no application. | `AccessCamera` from a sandboxed client shows a trusted prompt that names the requester (REQ-TRUST-011); no toolkit dialog appears. | vm | v1 | X, P |
| REQ-PERM-012 | A decision must be remembered only for a verified requester; when the requester identity is empty or unverified, the decision must apply to that one request and must not be stored. | The probe saw a refusal stored under the empty app id, which every unsandboxed program shares. | A request with an empty or self-declared app id leaves the permission store unchanged. | unit, vm | v1 | X, P |

## LAYOUT: windows and workspaces

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-LAYOUT-001 | The layout must be a pure function `step(state, event) -> (state, effects)` with time passed in as data. | It makes every rule testable and replayable. | The layout crate has no clock, I/O or randomness. | review, prop | v1 | D, G |
| REQ-LAYOUT-002 | A workspace must hold either one maximized window or two windows side by side, never more. | This is the two-slot invariant the overview and gestures rely on. | The invariant holds after every generated operation sequence. | prop | v1 | D, W |
| REQ-LAYOUT-003 | A new independent window must open on the active workspace if it is empty, or on a new workspace directly below it otherwise. | Predictable placement. An empty workspace makes room for the next window. | Opening an independent window reuses an empty active workspace or creates workspace n+1 when workspace n is occupied. | unit, client | v1 | W, B |
| REQ-LAYOUT-004 | Dialogs and popups must stay with their parent window. | They are part of the parent's task. | A transient maps on the parent's workspace, above the parent. | unit, client | v1 | W, B |
| REQ-LAYOUT-005 | Two columns must appear only by an explicit user command. | No client can split the view. | No client request produces a second column. | prop, client | v1 | W, B |
| REQ-LAYOUT-006 | Mapping or requesting activation may take the keyboard or change the active workspace only with a user action or a valid activation token from a real interaction. Without a token, a newly mapped dialog may inherit keyboard focus from its focused parent on the same Wayland connection, and a newly mapped independent window may take focus on the empty active workspace. Neither exception changes the active workspace. | It stops focus stealing and clickjacking. | Tokenless mapping changes keyboard focus only in the two listed cases. Tokenless activation, including for a dialog of another client, leaves the keyboard and active workspace unchanged. | unit, client | v1 | W |
| REQ-LAYOUT-007 | Workspaces must form one ordered vertical list with one active workspace. | It is the navigation model. | The reducer state has exactly this shape. | prop | v1 | D |
| REQ-LAYOUT-008 | The panel must take the top zone and windows the rest of the screen. | Matches today's bar. | A maximized window's geometry excludes the panel zone. | unit, client | v1 | D |
| REQ-LAYOUT-009 | The overview must be a projection of the layout state with no state of its own. | Two sources of truth drift apart. | Closing the overview without an action leaves the layout state unchanged. | prop | v1 | D |
| REQ-LAYOUT-010 | The system must support exactly one active output. `[open: O-15]` | Multi-monitor is out of scope. | The behavior when a second display is connected is defined and tested. | unit, hw | v1 | B |
| REQ-LAYOUT-011 | Output rotation on the Z13 must have one owner. `[open: O-14]` | The design names no owner for the output transform, and it must not depend on another compositor's IPC. | Rotating the tablet changes the output transform through the decided path. | hw | v1 | W |
| REQ-LAYOUT-012 | There must be no floating windows other than dialogs and popups. | Out of scope by decision. | No operation yields a free-floating independent window. | prop | v1 | D |
| REQ-LAYOUT-013 | The workspace list must end with exactly one empty workspace, and any other empty workspace must be removed as soon as it is not the active one. `[proposed]` | niri's rule for dynamic workspaces, without named workspaces: the next window always has a place and navigation never crosses a gap. | The invariant holds after every generated operation sequence; an emptied workspace survives until the user leaves it. | prop, unit | v1 | B |
| REQ-LAYOUT-014 | A fullscreen window must cover the whole output and hide its sibling column, only the focused column may be fullscreen, and leaving fullscreen must restore the columns as they were. `[proposed]` | The pair is remembered ([ux-niri-parity.md](ux-niri-parity.md)); a window that has the keyboard must be visible. | Toggling twice restores the state; focusing the sibling ends fullscreen; if the sibling closed meanwhile, the survivor gets the one-window layout. | unit, prop | v1 | B |
| REQ-LAYOUT-015 | Moving a window to the workspace above or below must never add a column: a lone window trades places with the neighboring workspace, and a column of a pair moves to a new workspace next to its old one. `[proposed]` | A move command must not become a second way to split (REQ-LAYOUT-005). | Both cases behave so in both directions; the empty last workspace keeps its place. | unit, prop | v1 | B |
| REQ-LAYOUT-016 | The layout must track at most a fixed number of windows and keep an application id only up to a fixed length. | Every size has a bound. | A window mapped beyond the limit gets a close request and no state; a longer id counts as absent. | unit | v1 | G |
| REQ-LAYOUT-017 | The effects of a step must report exactly what the step changed: each window's size, activation and fullscreen state, the keyboard focus, and the workspace list the shell shows. | The adapter and the shell see the layout only through effects; workspace and focus must reach the panel together. | Replaying the effects of any generated sequence reproduces every window's state and the focus, and no effect repeats what was already sent. | prop, unit | v1 | D |
| REQ-LAYOUT-018 | Window and workspace rectangles must be a pure projection of the layout state in logical output coordinates: tiles never overlap, a dialog has the slot of its tile, no window leaves the rectangle of its workspace, and the active workspace never leaves the output. | Rendering, hit testing and animation need one geometry. | The properties hold after every generated operation sequence, with the overview open and closed. | prop, unit | v1 | D |

## INPUT: input devices and routing

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-INPUT-001 | libinput must run inside the compositor process, with devices obtained from logind. | Decided 6 of 6; a helper would still see credential keystrokes. | The compositor opens no device node directly and needs no `input` group. | vm, review | v1 | W |
| REQ-INPUT-002 | Keyboard events must go only to the focused client or to the compositor itself; no interface may let another client observe keys. | Key observation interfaces on D-Bus have leaked passwords in other desktops. | No Wayland global or D-Bus service of Skorupa delivers key events to a non-focused client. | client, review | v1 | D, RS-01 |
| REQ-INPUT-003 | `text-input-v3` must be routed only to the trusted input method, and `input-method-v2` must be available only to the shell. | An input method sees and can inject text. | Another client cannot bind `input-method-v2`. | client | v1 | D |
| REQ-INPUT-004 | Compositor shortcuts must be handled before client delivery and must never be delivered to a client. | Clients must not see or swallow them. | A bound key combination reaches no client. | client | v1 | D |
| REQ-INPUT-005 | Attaching and detaching input devices must work at any time, including while locked and across suspend. | The Z13 keyboard is detachable. | Hotplug during lock and after resume leaves all devices working. | vm, hw | v1 | R1, G |
| REQ-INPUT-006 | Touch, touchpad gestures and keyboard must all work on the three target laptops. | These are the input devices in use. | The hardware checklist passes on each laptop. | hw | v1 | B |

## UX: parity with niri

Details live in [ux-niri-parity.md](ux-niri-parity.md); this table fixes only the behavior that must match. Reference: niri v26.04-324-g02ee0585.

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-UX-001 | Moving the pointer into the hot corner must toggle the overview. | Parity; it enables mouse-only window moves. | Behaves as in niri in the parity checklist. | client, hw | v1 | B, D |
| REQ-UX-002 | Alt+Tab must switch between windows in most-recently-used order, with Shift reversing. | Parity with niri's recent-windows switcher. | Order and commit behavior match the parity spec. | unit, client | v1 | B |
| REQ-UX-003 | Alt+` must switch between windows of the focused application, in most-recently-used order. | Parity (niri filters the same switcher by app id). | Only windows with the same app id are cycled. | unit, client | v1 | B |
| REQ-UX-004 | The overview must show the workspaces zoomed out and allow focusing and moving windows, opened by key, hot corner and gesture. | Parity. | The parity checklist for the overview passes. | client, hw | v1 | B, D |
| REQ-UX-005 | Touchpad gestures must match niri: three fingers vertically switch workspaces, four fingers vertically toggle the overview; three fingers sideways switch between the two columns of a workspace. | Parity, with one stated deviation: niri scrolls the view sideways. | The gesture checklist passes; gestures track the fingers and can be cancelled midway. | unit, hw | v1 | B, W |
| REQ-UX-006 | Screenshots must offer region selection, whole screen and single window, with copy to clipboard and save to disk. | Parity with niri's built-in screenshot actions. | Each of the three modes works from its shortcut (REQ-CAP-010). | client, vm | v1 | B |
| REQ-UX-007 | The user must be able to start and stop a screen recording from shortcuts, with a visible indicator while recording. `[open: O-18]` | Recording parity at start is an owner requirement. | Start, stop and indicator work; the file is playable. | vm, hw | v1 | B |
| REQ-UX-008 | Animations for workspace switch, overview and window open and close must use the same timing and curves as niri unless a difference is listed. | "Feels like niri" is an acceptance criterion. | Side-by-side recordings on the same laptop show no difference outside the list of REQ-UX-009. | unit, hw | v1 | B |
| REQ-UX-009 | Every intended difference from niri's behavior must be listed in [ux-niri-parity.md](ux-niri-parity.md). | Unlisted differences are bugs. | The owner signs off the list before migration. | review | v1 | B |
| REQ-UX-010 | Workspace commands must match niri with the owner's profile: a step up or down stops at the ends, a number beyond the list selects the last workspace, and selecting the current number or "previous workspace" returns to the workspace that was active before, if it still exists. | Parity. | The four commands behave so in the layout reducer. | unit | v1 | B |

## PERF: speed

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-PERF-001 | Input-to-photon latency must not be worse than niri 26.04 on the same laptop at p50, p95 and p99. `[open: O-16]` | The desktop must feel as fast as niri. | Alternating runs with a USB HID trigger and a photodiode show no regression beyond the agreed margin. | hw | v1 | D, B |
| REQ-PERF-002 | In transitions at 120 Hz and 180 Hz, Skorupa must not miss more frame deadlines than niri on the same laptop. `[open: O-16]` | Replaces the unprovable "zero dropped frames". | The same scripted transitions give a missed-deadline count not above niri's. | hw | v1 | W |
| REQ-PERF-003 | The frame schedule must be computed from actual presentation times, and animations must be sampled at the predicted presentation time. | A fixed timer drifts at 180 Hz. | With a simulated clock, late fences and late flips, the scheduler targets the right vblank. | unit | v1 | D |
| REQ-PERF-004 | The compositor must redraw only damaged regions and must use direct scanout on the primary plane and the cursor plane. | Both are needed for niri-class latency. | Damage tests show partial redraw; a fullscreen client is scanned out on each target GPU. | unit, hw | v1 | D |
| REQ-PERF-005 | The shell must not cap animations at 16 ms and must commit only changed regions of its surfaces. | Both limits exist in today's shell and are visible at 180 Hz. | Shell animation frames follow the output's refresh; commits carry damage smaller than the surface. | unit, hw | v1 | D |
| REQ-PERF-006 | v1 must support SDR output with dmabuf (including format feedback), explicit sync and direct scanout. | The agreed v1 graphics scope. | The compatibility list (Chromium, Firefox, GTK4, Qt6, mpv) renders correctly on AMD and Intel. | hw | v1 | W, B |
| REQ-PERF-007 | VRR must be qualified separately and be off unless enabled. | Panels must pass tests first. | A per-panel test record exists before VRR is enabled by default. | hw | later | W, B |
| REQ-PERF-008 | HDR and color management must be a separate decision after v1. | Smithay's color management was still a draft pull request on 2026-11-27. | A written decision exists before any HDR claim. | review | later | W, B |
| REQ-PERF-009 | The regression gate on hardware must cover workspace switch by key and by gesture, overview open and close, first use after idle, and the change from scanout to composition. | These are the transitions the user feels. | Each scenario has a recorded baseline and a pass threshold. | hw | v1 | D |
| REQ-PERF-010 | The page-flip timeout reported for 180 Hz AMD laptop panels must have its own test case. | A known driver problem on the main target. | The case runs on the Z13 and its result is recorded. | hw | v1 | D |
| REQ-PERF-011 | After a GPU reset, the compositor must rebuild its renderer and keep the lock state. | A reset must not crash into an unlocked or dead session. | An injected context loss leads to a new renderer generation with the lock unchanged. | unit, hw | v1 | D |
| REQ-PERF-012 | On an output whose flips complete without waiting for a vblank (virtio-gpu, the headless backend), a frame must still be presented at most once per refresh interval. | Unthrottled, virtio-gpu presented 6420 frames in 40 s, at a median interval of 1.70 ms. | With a simulated clock and flips that complete at once, the presentation times are exactly one refresh interval apart at 60 Hz and at 180 Hz. | unit, prop, client, vm | v1 | D |
| REQ-PERF-013 | A submitted frame whose flip does not complete within a set time must count as lost, and the output must redraw instead of waiting for it forever. | After a device resume on virtio-gpu no flip completed and the output stalled; flip timeouts are reported for the target laptop (REQ-PERF-010). | With a simulated clock, a flip that never completes ends in a timeout effect and a queued redraw; a flip that completes late is still taken; no order of events leaves a wait without a timer that ends it. | unit, prop, vm | v1 | D |
| REQ-PERF-014 | A surface must get at most one frame callback per refresh cycle, also when its commits cause no damage, and a surface that is not shown must still get one about once a second. | A client that commits on every callback without damage must not spin, and a hidden client must not stall. | The callback sequence advances once per submitted frame or estimated vblank; a client covered by another gets its callbacks about a second apart. | unit, client | v1 | D |
| REQ-PERF-015 | Presentation feedback must carry the time, refresh interval and sequence of the flip that showed the frame, and content that never reached the screen must not be reported as presented. | Clients pace video and animation from it (Chromium asks on nearly every commit, see [api-map.md](api-map.md)). | With a simulated clock, each feedback of a headless client carries exactly the time at which its flip was delivered; a covered or replaced commit is discarded. | client, hw | v1 | D |

## SIZE: code budget

Unit: `tokei`, Rust code lines, tests included, counted for the component they test. Quint, Lean and `.slint` are not counted.

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-SIZE-001 | The whole project must stay at or below 100,000 lines (target 96,000). | Hard cap set by the owner. | The gate fails above the cap. | gate | v1 | B |
| REQ-SIZE-002 | `compositor/` must stay at or below 35,000 lines. | Budget. | The gate reports the count against the target. | gate | v1 | B |
| REQ-SIZE-003 | `shell/` must stay at or below 24,000 lines. `[open: O-17]` | Budget; today's shell has about 42,000. | As above. | gate | v1 | B |
| REQ-SIZE-004 | `lock/` must stay at or below 17,000 lines. `[open: O-17]` | Budget; today's Rygiel has about 21,100 and gains polkit, consent and the bridge. | As above. | gate | v1 | B |
| REQ-SIZE-005 | `sandbox/` must stay at or below 7,000 lines. | Budget. | As above. | gate | v1 | B |
| REQ-SIZE-006 | `protocol/` must stay at or below 2,000 lines. | Budget; a small contract is the point. | As above. | gate | v1 | B |
| REQ-SIZE-007 | `sim/` and `fuzz/` together must stay at or below 6,000 lines. | Budget. | As above. | gate | v1 | B |
| REQ-SIZE-008 | The portal adapters (Screenshot and `Access`) and the CLI together must stay at or below 5,000 lines. `[open: O-31]` | Budget. The `Access` backend was not in the estimate. | As above. | gate | v1 | B, X |
| REQ-SIZE-009 | Going over a component target must need a recorded owner decision. | No silent growth. | The gate fails over target unless the decision is recorded next to the budget. | gate, review | v1 | D |

## DEP: dependencies and `unsafe`

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-DEP-001 | Smithay must be pinned to a full commit hash of upstream, with no patches. | Decided 6 of 6; a fork is a maintenance burden. | The manifest names an upstream revision; no `[patch]` entry exists for Smithay. | review | v1 | W, B |
| REQ-DEP-002 | Skorupa's own code must contain no `unsafe`: every crate must set `forbid(unsafe_code)`, with the single exception of the shell module that only invokes the Slint generator, and GPU bring-up must go through Smithay's safe `GbmGlesBackend`. | Smithay's EGL and GLES constructors are `unsafe`, but `GbmGlesBackend` calls them inside Smithay. Raw GL in Skorupa's own code needs a new owner decision before any `unsafe` is written. | A lint check finds `unsafe` only in the generated-code module, whose scope is described next to it. Evidence for the GPU path so far is a VM spike of bring-up (reported in [design.md](design.md)); the three laptops are unverified. | gate, review, hw | v1 | W, D, X |
| REQ-DEP-003 | No process must depend on tokio. | Smaller trusted base. | `cargo deny` bans it. | gate | v1 | D |
| REQ-DEP-004 | `lock/ui`, `authd` and `pwcheck` must not depend on Slint, a D-Bus library, serde or a TOML parser. | It keeps roughly half a million lines out of the trusted part. | The linked crate lists of these binaries contain none of them. | gate | v1 | D |
| REQ-DEP-005 | A known vulnerability in a dependency must block a merge; an unmaintained dependency must raise a warning with a replacement plan. | Policy in `deny.toml`. | The gate runs `cargo deny`; results are recorded with a date. | gate | v1 | D, W |
| REQ-DEP-006 | A new dependency must come with a written justification covering origin, license, advisories and the transitive crates it adds. | Each crate is trusted code. | The change that adds it contains the justification. | review | v1 | D, G |
| REQ-DEP-007 | The shell must use Slint with `std`, the CPU renderer and system fonts, and the production shell process must not link the Slint interpreter or compiler. | Embedded glyphs cannot show arbitrary titles; the interpreter adds about 78,000 lines to a process that sees notification text. | The shell's linked crate list has no `slint-interpreter` or `i-slint-compiler`. | gate | v1 | W |
| REQ-DEP-008 | Every Smithay update must be its own change with a review of the GPU boundary and a hardware test result. | Smithay has no stable API. | The update change contains the checklist and the test record. | review, hw | v1 | D |
| REQ-DEP-009 | Code ported from niri must be listed in a provenance ledger with file, upstream revision and authorship. | Both projects are GPL-3.0-or-later; attribution is owed. | Every ported file has a ledger row and a header. | review | v1 | W, B |
| REQ-DEP-010 | The versions of the C libraries in the trusted base (Mesa, libinput, libseat, libxkbcommon, libdrm, libudev, Linux-PAM, Pixman) must be recorded with every hardware test. | They are the largest trusted code and Rust does not cover them. | Each hardware test record has the list. | review | v1 | D |

## API: standard interfaces

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-API-001 | Applications must work without any knowledge of Skorupa. | Standards only. | Chromium, Firefox, a GTK4 app, a Qt6 app and mpv pass the compatibility checklist. | client, hw | v1 | D |
| REQ-API-002 | Every Wayland protocol must have a recorded decision in [api-map.md](api-map.md): for all, with a grant, or not implemented. | No protocol arrives by accident. | A global that is not in the map fails the snapshot test (REQ-GRANT-012). | client, review | v1 | D |
| REQ-API-003 | There must be no X11, XWayland, NVIDIA-specific or multi-output code. | Out of scope by decision. | No such dependency or module exists. | review | v1 | B, G |
| REQ-API-004 | Skorupa must expose no D-Bus interface that gives key events or screen content. | The lesson from GNOME, KWin and niri: D-Bus side doors defeat protocol allow-lists. | The list of owned bus names and interfaces contains none. | review, vm | v1 | D, RS-01 |
| REQ-API-005 | Each D-Bus name must have exactly one owning process, listed in [api-map.md](api-map.md): the bridge for logind and polkit, the shell for session services, the portal backend for the portal. | Ownership was missing from the design. | The map has the table; a VM test checks the owners. | review, vm | v1 | R2 |
| REQ-API-006 | The compositor must keep a bounded copy of the text selection so that paste works after the source application exits. `[open: O-22]` | Nobody gets `data-control`, so no clipboard manager can do it. | Copy, close the source, paste elsewhere: the text arrives; oversize content is not retained. | client | v1 | W |
| REQ-API-007 | Adapters must decide from protocol fields and documented exit codes, never from human-readable text. | Messages change with locale and version. | No adapter matches on message text. | review | v1 | G |
| REQ-API-008 | The portals in v1 must be ScreenCast, Screenshot, Camera (with the `Access` backend of REQ-PERM-011), FileChooser and Settings; RemoteDesktop must not be offered. | The agreed set, plus the backend the camera prompt needs. | The portal configuration lists exactly these. | vm | v1 | D, W, X |
| REQ-API-009 | Accessibility support must have a recorded stance before v1. `[open: O-13]` | The accessibility bus is a known side channel and also a real user need. | The decision is written in [threat-model.md](threat-model.md) and tested (REQ-SBX-001). | review | v1 | W |
| REQ-API-010 | A client buffer larger than the configured limit must be refused before its content is read: the commit must not take effect and the client must get a protocol error. | Every size has a bound; one client must not make the compositor draw or copy an arbitrarily large image. | A buffer of exactly the limit is shown; one pixel wider or taller gives `wl_surface.invalid_size`, is never drawn, and leaves other clients undisturbed. | client | v1 | G |
| REQ-API-011 | A client that disconnects at any point, also between its commit and the flip of that frame, must leave the compositor and the other clients running. | Clients crash; the frame in flight still holds their feedback objects. | A client closes its socket while its frame waits for the flip: the flip completes, the next window gets the focus, and the remaining client is paced and reported as before. | client | v1 | G |

## SHELL: panel, launcher, notifications

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-SHELL-001 | The shell must hold no password, no screen content and no unlock or lock-screen code. | "Zero unlock" in the shell. | The shell's domain has no capture global; no shell message reaches the unlock path. | client, review | v1 | D |
| REQ-SHELL-002 | A shell crash or hang must not affect the lock, the idle timer or running applications. | The shell is the largest process and the least trusted of the session parts. | Killing the shell leaves windows usable and the lock working; the shell restarts. | vm | v1 | D, W |
| REQ-SHELL-003 | Configuration must live in one `config.toml` with strict types; unknown keys must be rejected with a precise message; a bad reload must keep the previous configuration. | Few options, no silent typos. | An unknown key yields an error naming the key and line; the running shell is unchanged. | unit | v1 | D |
| REQ-SHELL-004 | Notification popups must be excluded from capture, and notification history must be bounded. | Private text in a shared screen; unbounded input from any app. | See REQ-CAP-004; a flood of notifications stays within the bound. | unit, client | v1 | W |
| REQ-SHELL-005 | Every shell service must recover from resume, a daemon restart and device hotplug by taking a fresh snapshot and resubscribing. | Stale state after resume is the common desktop bug. | Each service has a test for all three events on a private bus with fakes. | unit | v1 | G, D |
| REQ-SHELL-006 | The on-screen keyboard must be the only input method and must run as part of the shell. | One trusted input method (REQ-INPUT-003). | Text entry by touch works in ordinary applications. | client, hw | v1 | D |
| REQ-SHELL-007 | There must be no settings GUI. | Decided; configuration is the file and the CLI. | None exists. | review | v1 | D |

## PLUG: plugins

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-PLUG-001 | Built-in shell widgets must be compiled ahead of time. | No runtime compiler in the panel. | See REQ-DEP-007. | gate | v1 | W, B |
| REQ-PLUG-002 | A plugin `.slint` view must be loaded and hot-reloaded only in a separate process from the shell. `[open: O-12]` | The interpreter must not share a process with notification text. | The view process is a distinct PID with its own restricted environment. | vm, review | v1 | B |
| REQ-PLUG-003 | A plugin view process must load files only through an allow-list hook and must call only an allow-list of host functions. | A view file is code. | An import outside the plugin directory fails; an unlisted callback is not reachable. | unit | v1 | D |
| REQ-PLUG-004 | A data provider must be a separate process that writes JSON lines to stdout, sandboxed with no access to the session, with bounds on line size and rate. | Any language, no trust. | Oversized or too-frequent lines are dropped; the provider cannot reach session sockets. | unit, fuzz, vm | v1 | D, B |
| REQ-PLUG-005 | Reload must be transactional: the new view compiles and receives data before it replaces the old one, and a failure keeps the old one. | A typo must not blank the panel. | A broken view file leaves the previous view running. | unit | v1 | D |
| REQ-PLUG-006 | A provider that exits must be restarted with a bounded backoff, and its widget must show the stale state. | Bounded retries; visible failure. | Killing a provider repeatedly never exceeds the restart bound. | unit | v1 | G |
| REQ-PLUG-007 | Providers and TOML configuration must hot-reload without restarting the shell. | Decided. | Editing either takes effect without a shell restart. | unit | v1 | B |

## PROTO: the contract between processes

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-PROTO-001 | `protocol/` must be the only contract between Skorupa's processes, and must define messages only where no standard exists. | One small, fuzzed boundary. | No private message format exists outside `protocol/`. | review | v1 | D, G |
| REQ-PROTO-002 | Frames must be binary, length-limited and versioned, and a version mismatch must be rejected. | Fail closed on skew. | A frame over the limit or with another version is refused before allocation. | unit, fuzz | v1 | D |
| REQ-PROTO-003 | Every decoder must have a fuzz target and must never panic. | Decoders face less trusted peers. | Each decoder has a target in the gate's smoke run. | fuzz, kani | v1 | D |
| REQ-PROTO-004 | Every queue, size and retry in a channel must have a bound. | No unbounded work under load. | Each bound is a named constant with a test at the limit. | unit, kani | v1 | G |
| REQ-PROTO-005 | Protocol types that can carry a secret must not implement a `Debug` that prints it. | See REQ-AUTH-008. | A test formats each type and checks for the canary. | unit | v1 | G |
| REQ-PROTO-006 | A breaking change to `protocol/` must be detected by the gate. | All processes depend on it. | `cargo-semver-checks` runs on `protocol/`. | gate | v1 | D |
| REQ-PROTO-007 | Decoding an encoded message must return that message. | Round trip is the base contract. | Property test over all message kinds. | prop | v1 | D |
| REQ-PROTO-008 | Decoding must never panic or allocate, and an accepted frame must be canonical (re-encodes to the same bytes). | No padding or alternative spellings to smuggle data. | Property test and fuzz target compare re-encoding. | prop, fuzz | v1 | D |
| REQ-PROTO-009 | A frame must be at most `MAX_FRAME_LEN` bytes, checked on the length prefix before the body is read. | Bound work before reading. | Test at the limit and one byte over. | unit | v1 | D |
| REQ-PROTO-010 | A frame of another version must be refused whole. | Fail closed on skew. | Test for each wrong version byte. | unit | v1 | D |
| REQ-PROTO-011 | Part of a frame must never decode as a message; the error must say how many bytes are needed. | Streams arrive in pieces. | Test for every truncation point. | prop | v1 | D |
| REQ-PROTO-012 | The byte layout of every message must be fixed. | Peers in other builds must agree. | Golden-bytes test per message. | unit | v1 | D |
| REQ-PROTO-013 | Every field value outside the protocol must be refused with its own error; zero is never a valid version, kind or enum value. | Zeroed memory must not decode. | Test per field and value class. | unit, fuzz | v1 | D |
| REQ-PROTO-014 | Encoding into a buffer that is too small must be an error. | No partial writes. | Test with each short buffer length. | unit | v1 | D |

## OPS: verification, build and deployment

| Id | Requirement | Why | Accepted when | Proof | Stage | Src |
| --- | --- | --- | --- | --- | --- | --- |
| REQ-OPS-001 | The user account must not be in the `input` group. | Group members can read every keystroke from `/dev/input`, including the lock password. | A preflight check fails when the account is in the group. | vm, review | v1 | W, B |
| REQ-OPS-002 | No agent and no sandboxed process must be able to open `/dev/uinput`. | `uinput` creates keyboards that look real to the compositor. Where the node belongs to the `input` group, REQ-OPS-001 also closes it for the account itself. | The sandbox probe (REQ-SBX-002) confirms it; preflight reports who can open the node. | vm, review | v1 | W |
| REQ-OPS-003 | Yama `ptrace_scope` must be at least 1. | Without it, any process of the user can read the memory of the shell, portal and `authd`. | Preflight fails below 1. | vm, review | v1 | R2 |
| REQ-OPS-004 | The greeter must authenticate, with no automatic login. | REQ-LOCK-013 relies on it. | Preflight checks the greeter configuration. | review | v1 | W |
| REQ-OPS-005 | Trusted processes must not be dumpable, and privileged binaries must be installed only in root-owned paths. `[proposed]` | Cheap hardening against same-user tampering. | The processes set the flag at start; the package installs to root-owned paths. | vm, review | v1 | R1 |
| REQ-OPS-006 | A change must reach `main` only through the local gate, on exactly the commit that passed it. | There is no hosted CI. | The merge script refuses a commit other than the checked one. | gate, review | v1 | D |
| REQ-OPS-007 | The name of each test, simulation scenario, fuzz target and model invariant must contain the `REQ-` id it proves. | Traceability from this file to evidence. | A script lists ids without evidence and evidence without ids. | gate | v1 | D, G |
| REQ-OPS-008 | The simulator must compose the production lock, grant and `authd` reducers with injected crash, restart, delay, reorder, loss, sleep, output loss and grant expiry, and must return seed and trace on failure. | Narrowed scope agreed in the discussion. | A seeded failure reproduces from its seed. | sim | v1 | W |
| REQ-OPS-009 | A "sometimes" assertion that never fires in the simulator must fail the run. | An unreached assertion proves nothing. | The catalog of assertions is checked after each run. | sim | v1 | D |
| REQ-OPS-010 | There must be exactly one Quint model, covering lock epochs, presentation acknowledgement and revocation order. | One model for cross-process ordering, decided 6 of 6. | The model checks in the gate; counterexamples replay against Rust. | quint | v1 | W, B |
| REQ-OPS-011 | Kani proofs must cover the unlock gate, grant invalidation, queue bounds and the decoders. | Small, cheap, exhaustive within bounds. | The proofs run in the gate. | kani | v1 | D |
| REQ-OPS-012 | KMS, lock, VT and suspend tests must run on a test rig that is not a machine with a live user session. `[open: O-24]` | These tests need DRM master and real suspend, which a machine in use cannot give. | The rig exists and the hardware tests name it. | review | v1 | W |
| REQ-OPS-013 | VM tests must exist for the lock, the session and the agent boundary. | Real logind, PAM and processes. | `vm-lock`, `vm-session` and `vm-agent` run in the gate's full mode. | vm | v1 | D |
| REQ-OPS-014 | Each release must carry evidence for properties A and B: an unlock-effect test after broker disconnect and re-lock, distinguishable normal and protected content, presentation-versus-acknowledgement order across sleep, and KMS CRC where available. | These two properties are the product. | The release record contains the four results. | vm, hw | v1 | D |
| REQ-OPS-015 | niri must remain installed as the fallback session until Skorupa passes the hardware tests and one week of daily use without going back. | Migration safety. | The migration record shows both. | review | v1 | D, W |
| REQ-OPS-016 | Logs must be structured journald records and must contain no secret and, at the default level, no window title. | Logs are readable by more principals than the screen. | The canary tests of REQ-AUTH-008 and REQ-CAP-008 pass. | vm | v1 | D, W |
| REQ-OPS-017 | The threat model must be updated in the same change as any change to a trust boundary. | A stale threat model is worse than none. | The review checklist has the item. | review | v1 | D |
| REQ-OPS-018 | Dependency scan results must be stated with their date and scope, never as "zero vulnerabilities". | A scan is a snapshot. | Documents and release notes cite the dated result. | review | v1 | W |
| REQ-OPS-019 | The deployment preflight must report every installed helper that can capture the screen outside the compositor. `[proposed]` | A recorder with direct KMS access bypasses the compositor's exclusions and indicators. | Preflight lists such helpers; their disposition is recorded before migration. | review | v1 | X |

## Open decisions

Nothing below is decided. Requirements that carry an `[open: O-n]` marker state only the agreed part.

| # | Decision needed | Affects | Notes |
| --- | --- | --- | --- |
| O-1 | Is the reserved strip permanent, or shown only in trusted mode and on the reveal gesture? | REQ-TRUST-003 | A permanent strip costs fullscreen direct scanout; one reviewer rejects it, another proposes it. |
| O-2 | The device-provenance rule: what makes an input device "physical"? | REQ-TRUST-005 | Must reject `uinput` devices and still accept the detachable keyboard and the touch panel. |
| O-3 | Which key or touch gesture is the reveal gesture, and which is the kill switch? | REQ-TRUST-004, REQ-AGENT-006 | Must work with the keyboard detached. |
| O-4 | Deadline fallback: should the compositor turn the output off before logind's delay ends when no lock frame could be presented, and how does it learn the deadline? | REQ-LOCK-012 | The delay is a logind setting; read it at run time and never assume a value. The fallback narrows the exposure of REQ-LOCK-012 but cannot remove it when the inhibitor is lost. |
| O-5 | Threshold and sampling point for the `CLOCK_BOOTTIME` jump. | REQ-LOCK-016 | |
| O-6 | Backoff limits for `lock/ui` restarts, and the owner's recovery path when it never comes up. | REQ-LOCK-014 | One reviewer proposes a VT switch to a getty. Whether VT switching stays available while locked is part of this decision. |
| O-7 | Authentication methods in v1: password only, or also the FIDO2 and face paths Rygiel has? | REQ-AUTH-007, REQ-SIZE-004 | PAM multi-prompt conversations do not fit one 512-byte frame. |
| O-8 | Attempt throttling: left to PAM (`faillock`) or also enforced in `authd`? | REQ-AUTH-003 | Not addressed in the design or reviews. |
| O-9 | Which process drives the polkit agent session and its helper: the bridge or `authd`? | REQ-AUTH-012 | The design text says `authd`; the discussion outcome says the bridge registers the agent and polkit's helper completes. The password path from `lock/ui` to the helper is not drawn. |
| O-10 | Which D-Bus implementation does `lock/bridge` use? | REQ-AUTH-011, REQ-DEP-004 | The dependency decision says zbus only in the shell; the bridge needs D-Bus. |
| O-11 | Is the `security-context` global offered to ordinary applications so that Flatpak can create its own context? | REQ-GRANT-009 | The design gives it to the launcher only. Per one review, Flatpak then falls back to the ordinary socket and lands in the `app` domain (not re-checked here). |
| O-12 | Plugin views in a separate process: the design marks this "to be confirmed"; how is the view process's surface placed in the panel? | REQ-PLUG-002 | One review found no public Slint API to mount an interpreted view into a compiled panel. |
| O-13 | Accessibility: does the AT-SPI bus run, and who may reach it? | REQ-API-009 | |
| O-14 | Who owns output rotation on the Z13? | REQ-LAYOUT-011 | |
| O-15 | What happens when an external display is connected? | REQ-LAYOUT-010 | "One screen" is decided; the hotplug behavior is not. |
| O-16 | Performance limits and margins in milliseconds. | REQ-PERF-001, REQ-PERF-002 | To be set from the niri baseline measurement. |
| O-17 | The retirement list: which shell and lock code is removed to meet the budgets? | REQ-SIZE-003, REQ-SIZE-004 | Existing code is about 66,200 lines before the compositor. |
| O-18 | How does the user's own recording work, is region recording in v1, and are per-window capture-exclusion rules (niri's `block-out-from`) needed? | REQ-CAP-010, REQ-CAP-012, REQ-UX-007 | A recorder that reads KMS directly stays outside the compositor's exclusions and indicators (REQ-OPS-019). |
| O-19 | Is the user's screenshot a compositor action, the portal adapter, or both? Does an application's Screenshot request need consent each time? | REQ-CAP-007, REQ-CAP-010, REQ-UX-006 | |
| O-21 | May the compositor render text for its own indicators? | REQ-AGENT-007, REQ-TRUST-003 | The design says both "no text or fonts in the compositor" and "the compositor draws agent, window and rights". |
| O-22 | Limits of the retained clipboard copy (size, types, primary selection). | REQ-API-006 | |
| O-23 | Confirm or reject the `[proposed]` items. | REQ-LOCK-006, REQ-TRUST-013, REQ-OPS-005, REQ-OPS-019 | |
| O-24 | Which machine is the test rig, and who runs gates that need the Z13 panel? | REQ-OPS-012 | |
| O-25 | Which hardware keys stay active while locked (brightness, volume, power)? | REQ-LOCK-019 | |
| O-26 | What does the agent indicator show in v1, when no host grant can exist? | REQ-AGENT-007 | |
| O-27 | Font coverage of the consent renderer for untrusted strings. | REQ-TRUST-014 | |
| O-28 | Does an unanswered consent prompt time out as denied, and after how long? | REQ-TRUST-012 | Raised in one review as unspecified, together with prompt behavior over a fullscreen client. |
| O-29 | **Sleep hold.** After it acknowledges a lock for sleep, should the compositor refuse new attempts and verdicts until the sleep is over, with a new compositor instance starting in the hold? | REQ-LOCK-018, REQ-LOCK-013 | The Quint model found that a valid verdict between inhibitor release and suspend lets the machine sleep showing the session (finding 1), and checked the amended rule in simulation only. Costs: one more lock state, one more bridge message, every compositor start waits for the bridge, and a silent bridge can deny unlock. Recovery when the bridge never reports is undecided. The model's "a new instance starts covered" also differs from REQ-LOCK-013, where the session ends. |
| O-30 | Who holds the sleep inhibitor descriptor: the bridge, or the compositor after a hand-over? | REQ-LOCK-012, REQ-LOCK-018, REQ-LOCK-024 | While the bridge holds it, a bridge crash releases the delay. A hand-over would remove the acknowledgement message and its nonce. Not modelled. |
| O-31 | Owner and budget of the `Access` portal backend. | REQ-PERM-011, REQ-SIZE-008 | It is in neither the architecture nor the 5,000-line estimate. |

## Model ids

The Quint model names its rules with descriptive ids. This table maps them to the stable ids of this catalog; tests and invariants should carry the catalog id.

| Model id | Catalog id | Note |
| --- | --- | --- |
| REQ-LOCK-EPOCH | REQ-LOCK-007 | |
| REQ-LOCK-ACK-AFTER-PRESENT | REQ-LOCK-002 | first cause |
| REQ-LOCK-ACK-NO-OUTPUT | REQ-LOCK-002, REQ-LOCK-010 | second cause |
| REQ-LOCK-FLIP-FAIL-STAYS-LOCKED | REQ-LOCK-011 | |
| REQ-LOCK-COVER-WITHOUT-UI | REQ-LOCK-014 | |
| REQ-LOCK-UI-CANNOT-UNLOCK | REQ-LOCK-008 | |
| REQ-LOCK-LOGIND-UNLOCK-IGNORED | REQ-LOCK-017 | |
| REQ-LOCK-RESTART-STARTS-LOCKED | REQ-LOCK-013, REQ-AUTH-015 | The catalog ends the session; the model starts a new instance covered. See O-29. |
| REQ-LOCK-BOOTTIME-FIRST | REQ-LOCK-025 | |
| REQ-LOCK-RESUME-RELOCKS | REQ-LOCK-015, REQ-LOCK-016 | The model records submission; the catalog also rules out stale flips. |
| REQ-LOCK-SLEEP-AFTER-ACK | REQ-LOCK-018 | |
| REQ-LOCK-ACK-ECHOES-REQUEST | REQ-LOCK-024 | |
| REQ-LOCK-ACK-PROGRESS | none | Bounded progress check, not yet a requirement. |
| REQ-AUTH-VERDICT-BINDING | REQ-AUTH-001 | The model merges instance and session into one field; Rust must test each. |
| REQ-AUTH-VERDICT-ONE-USE | REQ-AUTH-001 | |
| REQ-AUTH-ATTEMPT-TIMEOUT | REQ-AUTH-004 | |
| REQ-AUTH-FRESH-UI | REQ-TRUST-002 | |
| REQ-AUTH-CRED-CHANNEL | REQ-AUTH-018 | |
| REQ-AUTH-FAIL-CLOSED | REQ-AUTH-003, REQ-AUTH-017 | |
| REQ-AUTH-RESUME-CLOSES-ATTEMPT | REQ-AUTH-016 | |
| REQ-AUTH-UNLOCK-PROGRESS | none | Bounded progress check, not yet a requirement. |
| REQ-GRANT-REVOKE-CLOSES-ALL | REQ-GRANT-005 | |
| REQ-GRANT-LOCK-REVOKES | REQ-LOCK-021, REQ-GRANT-006 | |
| REQ-GRANT-RESUME-REVOKES | REQ-GRANT-006 | |
| REQ-GRANT-CONSENT-BINDING | REQ-GRANT-015 | |
| REQ-GRANT-CHECK-AT-DISPATCH | REQ-GRANT-013 | |
| REQ-GRANT-EXPIRY-AT-DISPATCH | REQ-GRANT-013 | |
| REQ-GRANT-CONNECTION-BOUND | REQ-GRANT-014 | |
| REQ-GRANT-STOP | REQ-GRANT-008, REQ-PERM-004 | The model's Stop addresses the holder only; the catalog requires a generation. |
| REQ-CAP-WINDOW-GENERATION | REQ-GRANT-007 | |
| REQ-CAP-NO-FRAME-WHILE-LOCKED | REQ-LOCK-021 | |
| REQ-AGENT-LIVE-GRANT | REQ-AGENT-001 | |
| REQ-AGENT-ONE-SHOT | REQ-AGENT-005, REQ-CAP-011 | Window capture by an agent is after v1. |

## Deferred and out of scope

Listed so that nobody adds them by accident. A deferred item needs its own requirement before work starts.

| Item | State | Src |
| --- | --- | --- |
| Agent input into user windows (virtual keyboard or pointer on the host seat) | Not planned; the protocols address a whole seat | W, B |
| One-shot capture of a user window by an agent | After v1 (REQ-CAP-011) | W, B |
| RemoteDesktop portal | After v1 | D |
| Per-application microphone permission for sandboxed applications | After microphone activity is visible (REQ-PERM-006) | D, W |
| Clipboard secrets filter | Not in v1 | W |
| VRR | Right after v1, per panel (REQ-PERF-007) | B |
| HDR and color management | Separate decision after v1 (REQ-PERF-008) | W, B |
| Vulkan renderer | Re-evaluate when Smithay has a backend with dmabuf import, explicit sync, scanout and reset recovery | D |
| Overlay planes | After measurement on AMD and Intel | D |
| libinput in its own process | Dropped as a milestone; reopen only for a shown containment gain | W |
| A separate GPU process | Prototype only after measurements | D |
| Raw GL effects in Skorupa's own code | Needs a new owner decision before any `unsafe` is written (REQ-DEP-002) | D, X |
| Slint interpreter inside the shell process | Not in the production shell (REQ-DEP-007) | W |
| Untrusted third-party plugin views | Later, and only in a confined process | D |
| A user secret or seal shown in trusted prompts | Optional; the strip and gesture are the baseline | R2 |
| Full proof pilot (Verus, Creusot or Aeneas) | Last, for one authorization kernel | D |
| NVIDIA, X11, XWayland, several screens at once, floating windows, a settings GUI | Out of scope (REQ-API-003, REQ-LAYOUT-010, REQ-LAYOUT-012, REQ-SHELL-007) | B, D |

## Evidence checked for this catalog

Claims this catalog relies on, re-checked on 2026-11-27 against source. Anything not listed here is taken from the design document or the reviews and is unverified by this catalog. Readings from any particular machine are deliberately not recorded in this repository.

| Claim | Verdict | Where |
| --- | --- | --- |
| Smithay sets its lock status to unlocked and then calls the compositor's `unlock()` when the lock client sends `unlock_and_destroy`. | True | Smithay `46643d20`, `src/wayland/session_lock/lock.rs:117-124` |
| Rygiel's lock client sends `unlock_and_destroy` today. | True | Rygiel `crates/rygiel/src/app.rs:899-901` |
| niri unlocks when a locked frame fails to render during locking. | True | niri v26.04-324-g02ee0585, `src/niri.rs:5143-5146` |
| Smithay's EGL display and GLES renderer constructors are `unsafe`. | True | `src/backend/egl/display.rs:243`, `src/backend/renderer/gles/mod.rs:575` |
| Smithay's Pixman renderer has a safe constructor. | True | `src/backend/renderer/pixman/mod.rs:714` |
| GPU bring-up through `GbmGlesBackend` needs no `unsafe` in Skorupa's code. | Reported (VM spike described in design.md); not re-checked here | — |
| Rygiel's PAM worker runs the account check and compares the PAM user. | True | Rygiel `crates/rygiel-pwcheck/src/pam.rs:42-66` |
| xdg-desktop-portal-wlr tries restore data before the chooser. | True | v0.9.0, `src/screencast/screencast.c:163-166` |
| It logs every capturable window's title and app id at INFO. | True | v0.9.0, `src/screencast/screencast.c:151-152` |
| Its chooser code never receives the requester's app id. | True | v0.9.0, `src/screencast/chooser.c` has no `app_id` reference |
| Its Screenshot runs `grim` and writes `/tmp/out.png`. | True | v0.9.0, `src/screenshot/screenshot.c:20-25,120-121` |
| niri implements a keyboard-monitor D-Bus interface for screen readers. | True | niri `src/dbus/freedesktop_a11y.rs:109` |
