# Cross-process ordering model

A [Quint](https://quint-lang.org) model of the order of events between the
compositor, the lock UI, `authd`, `pwcheck`, logind with its bridge, and the
holders of capture and agent grants. It was written before any implementation
and is a proposal for the contract in `protocol/`.

The model answers one question: which rules must each process follow so that
no interleaving of messages, crashes, failed page flips and suspends unlocks
the session, shows it, or delivers it to a client that has lost its grant.

- `lock.qnt`: the model, its invariants, tests and counterexample traces.
- `variants.qnt`: the chosen design for v1 (`chosen`), 20 variants that each
  remove one rule, the post-v1 option `post_v1_agent_capture`, and one
  candidate rule (`option_sleep_hold`). One of the 20 is the rejected design
  in which the lock client unlocks the session itself.

## Results in short

- The chosen design passes every invariant in every run recorded below. The
  model changed after a review; the large runs are from the revision before
  it, and the current revision has one smaller batch (see
  [Recorded runs](#recorded-runs)).
- All 20 variants are refuted, each by a random simulation and by a short
  hand-written trace. The rejected `ext-session-lock` design is refuted in
  4 steps, and Apalache confirms that no shorter trace exists.
- The chosen design does **not** guarantee that the machine only sleeps with
  the panel covered, even after it acknowledged the lock for that sleep.
  A candidate rule for the acknowledged case is modelled. Its first version
  was refuted by the simulator; the amended one passed 400 000 traces.
  See [Findings](#findings).

## How to run

Quint is not installed on the development machine. Take it from nixpkgs:

```sh
alias quint="nix --extra-experimental-features 'nix-command flakes' shell nixpkgs#quint -c quint"
cd spec
```

The nixpkgs package (0.33.1 on 2026-11-27) bundles the Rust simulator,
Apalache 0.62.4 and a JDK, so `quint verify` needs no Java on the host.

```sh
# Types and names.
quint typecheck variants.qnt

# Scenario, guard and progress tests on the chosen design (31 tests).
quint test --main=chosen variants.qnt

# Random simulation of the chosen design against all invariants.
quint run variants.qnt --main=chosen --invariant=safety \
  --max-samples=100000 --max-steps=100 --n-threads=4

# The same with a per-invariant report and witness counts.
quint run variants.qnt --main=chosen --max-samples=100000 --max-steps=100 --n-threads=4 \
  --invariants propA_verdict propA_pam propA_typed propA_wake propA_broker \
    propB_covered propB_phase propB_resume propS_sleep propC_noLateDelivery \
    propC_lockRevokes propC_consent propC_stopExact propD_agent propD_v1 \
  --witnesses witUnlock witAck witGrant witDelivery witDrop witSleepByAck \
    witNegativeVerdict witSecondEpoch witSecondInstance witUnlockAfterResume \
    witStaleFlip witStop witTransferGrant reachInst reachEpoch reachAttempt \
    reachAsk reachGrant reachJob reachWin reachConn reachSleep

# Fault storm: every group of actions equally likely.
quint run variants.qnt --main=chosen --step=stepStorm --invariant=safety \
  --max-samples=50000 --max-steps=100 --n-threads=4

# The rejected design: the simulator must report a violation of propA_verdict.
quint run variants.qnt --main=rejected_client_unlock --invariant=safety

# Its hand-written counterexample as a test. Every variant has one.
quint test --main=rejected_client_unlock --match=counterexample variants.qnt

# Bounded symbolic check with Apalache (heavy: run it through the job queue).
quint verify variants.qnt --main=chosen --invariant=safety --max-steps=6

# Explicit-state search with TLC (heavy; not exhausted in 15 minutes).
quint verify variants.qnt --main=chosen --invariant=safety --backend=tlc
```

A violation prints the trace and a seed. `--seed=<seed>` replays it;
`--mbt --out-itf=trace.itf.json` writes it with the action name and the
nondeterministic picks of every step.

`quint test` without `--match` runs definitions whose name ends in `Test`.
On a variant other than `chosen` the guard and progress tests are expected to
fail; run only `--match=counterexample` there.

## Scope

Processes and what the model keeps of each:

| Process | Modelled | Left out |
| --- | --- | --- |
| compositor | lock phase, epoch, attempt, instance id, consent prompt, grant table; restart loses all of it | layout, input routing, rendering |
| kernel (KMS) | frame on the panel, one pending page flip, flip success and failure, output off and on; a flip pending at suspend is shown before the sleep or dropped, and its event arrives after the resume | modes, planes, more than one output |
| `lock/ui` | one process and one credential channel per attempt; crash; the secret it forwards is right or wrong | drawing, keyboard handling |
| `authd` | current challenge, one credential per challenge, restart loses both and closes the compositor's open attempt | polkit sessions, rate limiting |
| `pwcheck` | one PAM result per request; a crash is a missing result | PAM itself |
| logind and bridge | sleep announcement, delay inhibitor, its timeout, suspend, resume, bridge crash and restart, `Unlock` signal | idle hints, `Lock` as a separate signal (folded into `compLock`), polkit |
| portal, agent | one grant per holder, bound to a window generation and a connection; reconnect; queued delivery effects; Stop requests that name a stream generation. Holders: portal stream, agent clipboard transfer, and agent capture of a host window, which exists only with `AGENT_CAPTURE = true` (after v1) | PipeWire, the content of frames, clipboard data |
| clocks | "CLOCK_BOOTTIME ran ahead of CLOCK_MONOTONIC" as one flag set by resume; a grant deadline that passes | numeric time |

Faults: every message stays deliverable forever, which covers loss,
duplication, reordering and messages that outlive a restart of either end;
crash and restart of compositor, `lock/ui`, `authd`, bridge, portal and agent;
a missing `pwcheck` result; failed render, failed page flip, output off;
inhibitor timeout; suspend at any point where logind allows it.

Trust: the compositor, `authd`, `pwcheck` and the bridge follow their rules
but may crash. `lock/ui` may send anything its channel allows, at any time. In
the chosen design that is a credential for its own attempt; in the rejected
design it is also the unlock request. A bridge that misbehaves can only drop
its inhibitor, which `bridgeCrash` covers. Code running unsandboxed on the
user's account is out of scope, as in the design.

Abstractions that matter when reading a result:

- **Instance and session are one field.** `inst` stands for the pair
  (compositor instance, logind session), because a compositor restart ends
  the session and the two change together. The model therefore cannot find a
  bug in which only one of the two is checked. The Rust reducer tests must
  cover each field on its own: a verdict with the right instance and a wrong
  session, and the reverse.
- The consent UI is trusted to report the user's choice. Who interprets the
  physical confirmation is an open question of the threat model, not
  something this model decides.
- An `authd` restart and the compositor's reaction to the closed connection
  are one step. That is exact for a stream socket (nothing is read after the
  end of the stream); it would not be for a datagram or bus transport.
- The kernel is assumed to settle a pending page flip when the machine goes
  to sleep, so that no frame submitted before the sleep reaches the panel
  after it. This was not checked on the target machines.
- `compLock` stands for every lock trigger: shortcut, idle, logind `Lock`,
  and the lock request the bridge sends for a sleep.
- The lock frame is drawn by the compositor from the UI surface or, without
  one, as a plain cover. The model does not distinguish the two.
- The page-flip event and the compositor's reaction to it are one step.
- Bounds of `step`, and the share of the 40 000 traces of the current
  revision's batch that reached each one:

  | Bound | Value | Reached in |
  | --- | --- | --- |
  | compositor instances | 3 | 72.55 % |
  | lock epochs requested per instance (a resume can add one) | 3 | 3.33 % |
  | unlock attempts per epoch | 2 | 98.66 % |
  | consent prompts per instance | 2 | 47.87 % |
  | grants per run | 3 | 0.36 % |
  | delivery effects per run | 3 | 6.13 % |
  | window generations | 2 | 90.56 % |
  | reconnects per holder | 1 | 92.25 % |
  | sleep cycles | 2 | 99.99 % |

  Nothing beyond these values was explored: no fourth instance, no third
  sleep cycle, no third attempt in one epoch, one user window, one output.

## Properties

`safety` is the conjunction of the invariants below.

| Property | Invariant | Meaning | Requirements |
| --- | --- | --- | --- |
| A | `propA_verdict` | every unlock was caused by a positive `authd` verdict for the instance, epoch and open attempt current at that moment | REQ-AUTH-001, REQ-AUTH-004, REQ-LOCK-008, REQ-LOCK-017, REQ-LOCK-011 |
| A | `propA_pam` | `authd` issues a positive verdict only after PAM succeeded for the same binding | REQ-AUTH-003 |
| A | `propA_typed` | the user typed the right secret into the UI of the attempt that unlocked | REQ-AUTH-018, REQ-TRUST-002 |
| A | `propA_wake` | no unlock on an attempt opened before the last resume | REQ-AUTH-016, REQ-LOCK-025 |
| A | `propA_broker` | no unlock on a verdict issued by an `authd` that has disconnected since | REQ-AUTH-017, REQ-AUTH-004 |
| B | `propB_covered` | from the lock acknowledgement until an unlock by verdict the panel never shows session content | REQ-LOCK-002, REQ-LOCK-010, REQ-LOCK-014, REQ-LOCK-013 |
| B | `propB_phase` | `Locked` in the compositor means acknowledged, with no session frame pending | REQ-LOCK-002 |
| B | `propB_resume` | after a resume no session frame is submitted and none is presented before an unlock by verdict in the same wake period; the first frame that reaches the panel after a resume is a lock frame | REQ-LOCK-015, REQ-LOCK-016, REQ-LOCK-025 |
| sleep | `propS_sleep` | the bridge releases the delay inhibitor on an acknowledgement only if the compositor was `Locked` for that request | REQ-LOCK-018, REQ-LOCK-024 |
| C | `propC_noLateDelivery` | nothing is delivered on a revoked grant; lock, resume, restart, Stop, expiry, a replaced window, a reconnect and the use of a one-shot grant are revocations | REQ-GRANT-013, REQ-GRANT-014, REQ-GRANT-008, REQ-GRANT-006, REQ-GRANT-007, REQ-LOCK-021 |
| C | `propC_lockRevokes` | outside `Unlocked` the compositor holds no live grant and no open prompt | REQ-LOCK-021, REQ-GRANT-005 |
| C | `propC_consent` | every grant has the user's allow for exactly that prompt | REQ-GRANT-015 |
| C | `propC_stopExact` | a Stop ends only the stream generation it names | REQ-GRANT-008 |
| D | `propD_agent` | an agent receives capture or a transfer only on a live grant, once per grant | REQ-AGENT-001, REQ-AGENT-005, REQ-CAP-011 |
| D | `propD_v1` | without `AGENT_CAPTURE` no prompt, grant or delivery for agent capture of a host window exists | REQ-AGENT-001 |

Liveness is checked as bounded progress, not as a temporal formula. Each test
runs 40 random steps with faults and then a fixed fault-free schedule:

| Test | Claim | Requirement |
| --- | --- | --- |
| `lockAckProgressTest` | a lock request is acknowledged, the panel is covered and a pending sleep is released within 12 steps | lockAckProgress (proposed) |
| `unlockProgressTest` | with the right secret the session unlocks within 20 steps | unlockProgress (proposed) |

Witnesses (`wit*`) show that the invariants are not vacuous: a verified
unlock, an acknowledgement, a grant, a delivery, a dropped delivery, a sleep
released by acknowledgement, a negative verdict, a second epoch, a second
compositor instance and an unlock after a resume are all reached.

Two stronger statements are in the model but outside `safety`, because the
chosen design does not meet them: `strongS_sleepCovered` and
`strongS_ackedSleepCovered`.

## Variants

Each variant removes one rule. "Steps" is the length of the fixed
counterexample `cx_<defect>` in `lock.qnt`; the guard test replays the same
trace on the chosen design.

| Module | Rule removed (`DEFECT`) | Steps | Violates | Guard test on `chosen` |
| --- | --- | --- | --- | --- |
| `rejected_client_unlock` | the lock client cannot unlock (`client_unlock`) | 4 | `propA_verdict` | `uiCannotUnlockGuardTest` |
| `ablate_logind_unlock` | logind `Unlock` is ignored (`honor_logind_unlock`) | 4 | `propA_verdict` | `logindUnlockIgnoredGuardTest` |
| `ablate_render_error` | a failed lock frame keeps the lock (`unlock_on_render_error`) | 2 | `propA_verdict` | `renderErrorStaysLockingGuardTest` |
| `ablate_instance` | verdict bound to the instance (`skip_instance_check`) | 15 | `propA_verdict` | `staleInstanceVerdictGuardTest` |
| `ablate_epoch` | verdict bound to the epoch (`skip_epoch_check`) | 15 | `propA_verdict` | `staleEpochVerdictGuardTest` |
| `ablate_attempt` | verdict bound to the open attempt (`skip_attempt_check`) | 11 | `propA_verdict` | `lateVerdictAfterTimeoutGuardTest` |
| `ablate_ui_channel` | one credential channel per attempt (`shared_ui_channel`) | 12 | `propA_typed` | `staleCredentialGuardTest` |
| `ablate_ack_on_submit` | acknowledge on presentation, not on submit (`ack_on_submit`) | 2 | `propB_covered` | `happyPathTest` |
| `ablate_ack_any_flip` | acknowledge only on the lock frame's flip (`ack_any_flip`) | 3 | `propB_covered` | `staleFlipDoesNotAckTest` |
| `ablate_boottime` | clock jump handled before any event (`no_boottime`) | 5 | `propB_resume` | `noFrameBeforeJumpGuardTest` |
| `ablate_resume_attempt` | resume closes the open attempt (`keep_attempt_on_resume`) | 14 | `propA_wake` | `verdictAcrossSleepGuardTest` |
| `ablate_stale_ack` | acknowledgement echoes the request nonce (`stale_ack`) | 21 | `propS_sleep` | `staleLockAckGuardTest` |
| `ablate_lock_revokes` | lock revokes grants (`no_revoke_on_lock`) | 4 | `propC_lockRevokes` | `queuedDeliveryDroppedByLockTest` |
| `ablate_dispatch_check` | grant checked when the effect runs (`check_at_enqueue_only`) | 6 | `propC_noLateDelivery`, `propD_agent` | `queuedDeliveryDroppedByLockTest`, `agentOneShotTest` |
| `ablate_grant_by_name` | grant bound to a connection (`grant_by_name`) | 6 | `propC_noLateDelivery` | `reconnectDropsDeliveryGuardTest` |
| `ablate_timer_expiry` | deadline read at dispatch (`timer_only_expiry`) | 6 | `propC_noLateDelivery` | `expiredDeliveryDroppedGuardTest` |
| `ablate_window_gen` | grant bound to the window generation (`skip_window_gen`) | 6 | `propC_noLateDelivery` | `replacedWindowDropsDeliveryGuardTest`, `consentForReplacedWindowGuardTest` |
| `ablate_broker_disconnect` | `authd` disconnect closes the open attempt (`keep_attempt_on_authd_disconnect`) | 11 | `propA_broker` | `verdictAfterBrokerDisconnectGuardTest` |
| `ablate_stale_flip` | a flip from before the sleep acknowledges nothing (`ack_stale_flip`) | 7 | `propB_covered` | `staleFlipAfterResumeGuardTest` |
| `ablate_stop_generation` | Stop names the stream generation (`stop_by_holder`) | 8 | `propC_stopExact` | `staleStopGuardTest` |

`option_sleep_hold` is not in this table: it adds a rule instead of removing
one, and it is not part of the chosen design (finding 1).
`post_v1_agent_capture` is the chosen design with `AGENT_CAPTURE = true`; all
invariants must hold there, and `agentCaptureGateTest` checks the one-shot
capture there and the refusal in `chosen`.

The rejected design, step by step (`cx_client_unlock`):

1. `compLock`: the compositor opens epoch 1.
2. `compRender(true)`: it submits the lock frame.
3. `kmsFlipDone`: the frame is on the panel; the lock is acknowledged.
4. `uiUnlockAndDestroy`: the lock client sends `unlock_and_destroy`. The
   session is unlocked. `authd` was never asked.

This is what the protocol allows today: Smithay sets its state to unlocked
before it calls the compositor (`src/wayland/session_lock/lock.rs:122` at the
candidate revision), and the current lock client sends that request itself.

## Recorded runs

All runs are from 2026-11-27 with Quint 0.33.1 from nixpkgs (Rust evaluator
0.7.1, Apalache 0.62.4). The model has two revisions. A review changed four
things: an `authd` disconnect closes the open attempt, flips left over from
before a sleep are modelled, Stop names a stream generation, and agent
capture of a host window is an option. The large runs below are from the
revision before those changes and do not carry over formally.

### Current revision

One batch, kept short. Neither Apalache nor TLC was run on this revision.

Tests:

| Command | Result |
| --- | --- |
| `quint test --main=chosen variants.qnt` | 31 passing; the two progress tests ran 10 000 samples each |
| `quint test --main=post_v1_agent_capture variants.qnt` | 31 passing |
| `quint test --main=<variant> --match=counterexample variants.qnt`, 20 variants | 21 passing (one variant has two traces) |
| `quint test --main=chosen --match=cx_ variants.qnt` | 21 failing, as intended |
| `quint test --main=option_sleep_hold --match='ProgressTest\|sleepHold.*Test' variants.qnt` | 4 passing |

Simulation, all 15 invariants of `safety`, 100 steps per trace, one thread:

| Module | Step action | Samples | Seed | Replay seed | Result | Time |
| --- | --- | --- | --- | --- | --- | --- |
| `chosen` | `step` | 40 000 | `0x21` | `0x83507e9` | no violation | 194 s |
| `chosen` | `stepStorm` | 10 000 | `0x24` | `0x199164a` | no violation | 35 s |
| `post_v1_agent_capture` | `step` | 10 000 | `0x22` | `0x20dc6e9` | no violation | 45 s |
| `option_sleep_hold`, plus `strongS_ackedSleepCovered` | `step` | 10 000 | `0x23` | `0x21b3bd5` | no violation | 46 s |

That is 50 000 traces and 5 million steps for `chosen`. Share of the 40 000
`step` traces that reached each witness: `witAck` 100.00 %, `witSleepByAck`
65.99 %, `witStaleFlip` 47.65 %, `witGrant` 38.97 %, `witNegativeVerdict`
36.68 %, `witUnlock` 34.84 %, `witUnlockAfterResume` 30.75 %, `witSecondEpoch`
29.71 %, `witDrop` 15.54 %, `witTransferGrant` 14.45 %, `witDelivery` 11.38 %,
`witStop` 4.11 %, `witCaptureGrant` 0 % (as required in v1; 14.65 % in
`post_v1_agent_capture`). The bounds reached are in [Scope](#scope); the
grant bound was reached in 0.36 % of the traces and the epoch bound in
3.33 %, so behaviour at those two bounds is thinly sampled.

Variants, random simulation with `step` (100 000 samples allowed). Every run
stopped at its first violation, in under 2 s:

| Module | Seed | Violated | Replay seed |
| --- | --- | --- | --- |
| `rejected_client_unlock` | `0x30` | `propA_verdict` | `0x30` |
| `ablate_logind_unlock` | `0x31` | `propA_verdict` | `0x31` |
| `ablate_render_error` | `0x32` | `propA_verdict` | `0xfce3` |
| `ablate_instance` | `0x33` | `propA_verdict` | `0x2ac5` |
| `ablate_epoch` | `0x34` | `propA_verdict`, `propA_typed` | `0x2677` |
| `ablate_attempt` | `0x35` | `propA_verdict`, `propA_wake` | `0x1982` |
| `ablate_ui_channel` | `0x36` | `propA_typed` | `0x93c3` |
| `ablate_ack_on_submit` | `0x37` | `propB_covered` | `0x1add` |
| `ablate_ack_any_flip` | `0x38` | `propB_covered` | `0x38` |
| `ablate_boottime` | `0x39` | `propA_wake` | `0x39` |
| `ablate_resume_attempt` | `0x3a` | `propA_wake` | `0x1a29` |
| `ablate_stale_ack` | `0x3b` | `propS_sleep` | `0x1b312` |
| `ablate_lock_revokes` | `0x3c` | `propC_lockRevokes` | `0xec5` |
| `ablate_dispatch_check` | `0x3d` | `propC_noLateDelivery` | `0x86c0` |
| `ablate_grant_by_name` | `0x3e` | `propC_noLateDelivery` | `0x1462d1` |
| `ablate_timer_expiry` | `0x3f` | `propC_noLateDelivery`, `propD_agent` | `0xcc3a9` |
| `ablate_window_gen` | `0x40` | `propC_noLateDelivery` | `0x1d9ec` |
| `ablate_broker_disconnect` | `0x41` | `propA_broker` | `0x10cec` |
| `ablate_stale_flip` | `0x42` | `propB_covered` | `0x8aa7c` |
| `ablate_stop_generation` | `0x43` | `propC_stopExact` | `0x102167` |

### Revision before the review

The invariant count there was 12, the test count 24 and the variant count 17.
The simulations of `chosen` and of the 17 variants and the TLC run used the
model as first committed; the change after that touched only the option
`sleep_hold` and one comment. The amended option and the Apalache runs to 5
and 6 steps used the model after that change. With `--seed` the
simulator ran on one thread, at 70 to 260 traces per second on a shared
machine. "Replay seed" is the seed Quint printed at the end of the run.

Tests:

| Command | Result |
| --- | --- |
| `quint test --main=chosen variants.qnt` | 24 passing; the two progress tests ran 10 000 samples each |
| `quint test --main=<variant> --match=counterexample variants.qnt`, 17 variants | 18 passing (one variant has two traces) |
| `quint test --main=chosen --match=cx_ variants.qnt` | 18 failing, as intended: no counterexample trace runs on the chosen design |

Chosen design, all 12 invariants of `safety`:

| Step action | Samples | Steps per trace | Seed | Replay seed | Result | Time |
| --- | --- | --- | --- | --- | --- | --- |
| `step` | 100 000 | 100 | `0x1` | `0x136d50a9` | no violation | 422 s |
| `stepStorm` | 50 000 | 100 | `0x2` | `0x788ef1f` | no violation | 190 s |
| `step` | 20 000 | 300 | `0x3` | `0xc69e52a` | no violation | 286 s |

That is 170 000 traces and 21 million steps. Share of traces that reached
each witness:

| Witness | `step`, 100 steps | `stepStorm`, 100 steps | `step`, 300 steps |
| --- | --- | --- | --- |
| `witAck` | 100.00 % | 100.00 % | 100.00 % |
| `witUnlock` | 41.59 % | 18.99 % | 59.13 % |
| `witUnlockAfterResume` | 37.58 % | 18.33 % | 56.53 % |
| `witNegativeVerdict` | 43.94 % | 19.31 % | 71.07 % |
| `witSecondEpoch` | 35.20 % | 17.68 % | 57.12 % |
| `witSecondInstance` | 93.13 % | 99.84 % | 100.00 % |
| `witSleepByAck` | 66.49 % | 44.08 % | 65.75 % |
| `witGrant` | 39.95 % | 15.34 % | 47.15 % |
| `witDelivery` | 11.65 % | 1.04 % | 16.00 % |
| `witDrop` | 16.06 % | 3.29 % | 21.73 % |

Chosen design, the two stronger statements (100 000 samples allowed, 100 steps):

| Invariant | Seed | Result | Replay seed |
| --- | --- | --- | --- |
| `strongS_sleepCovered` | `0x4` | violated in the first 0.2 s | `0x3dfb` |
| `strongS_ackedSleepCovered` | `0x5` | violated after 1.0 s, about 280 traces | `0xdbc77` |

Variants, random simulation with `step` (100 000 samples allowed, 100 steps).
Every run stopped at its first violation, in under 0.3 s:

| Module | Seed | Violated | Replay seed |
| --- | --- | --- | --- |
| `rejected_client_unlock` | `0x10` | `propA_verdict` | `0x10` |
| `ablate_logind_unlock` | `0x11` | `propA_verdict` | `0x11` |
| `ablate_render_error` | `0x12` | `propA_verdict` | `0xd401` |
| `ablate_instance` | `0x13` | `propA_verdict`, `propA_typed` | `0xcba` |
| `ablate_epoch` | `0x14` | `propA_verdict`, `propA_typed` | `0xc06` |
| `ablate_attempt` | `0x15` | `propA_verdict` | `0x5874` |
| `ablate_ui_channel` | `0x16` | `propA_typed` | `0x10ce6` |
| `ablate_ack_on_submit` | `0x17` | `propB_covered` | `0xdbb` |
| `ablate_ack_any_flip` | `0x18` | `propB_covered` | `0x33b0` |
| `ablate_boottime` | `0x19` | `propB_resume` | `0x19` |
| `ablate_resume_attempt` | `0x1a` | `propA_wake` | `0x56dc` |
| `ablate_stale_ack` | `0x1b` | `propS_sleep` | `0xd73` |
| `ablate_lock_revokes` | `0x1c` | `propC_lockRevokes` | `0x1c` |
| `ablate_dispatch_check` | `0x1d` | `propC_noLateDelivery` | `0x182d` |
| `ablate_grant_by_name` | `0x1e` | `propC_noLateDelivery`, `propD_agent` | `0x1e` |
| `ablate_timer_expiry` | `0x1f` | `propC_noLateDelivery`, `propD_agent` | `0x1d508` |
| `ablate_window_gen` | `0x20` | `propC_noLateDelivery` | `0x68a9` |

Candidate rule `option_sleep_hold`, `safety` plus `strongS_ackedSleepCovered`:

| Version | Step action | Samples | Steps per trace | Seed | Result |
| --- | --- | --- | --- | --- | --- |
| first: hold kept in memory only | `step` | 50 000 allowed | 100 | `0x6` | `strongS_ackedSleepCovered` violated at trace 20 237 after 94 s, replay seed `0x3fe2b2a`; the 12 invariants of `safety` held |
| amended: a new instance starts in the hold | `step` | 300 000 | 100 | none, 4 threads | no violation in 331 s, replay seed `0x3c2505979c6a1bea` |
| amended | `stepStorm` | 100 000 | 100 | none, 4 threads | no violation in 87 s, replay seed `0x20ace6633ab49b4b` |

`quint test --main=option_sleep_hold --match='ProgressTest|sleepHold.*Test' variants.qnt`:
4 passing, the two progress tests with 10 000 samples each.

Symbolic and explicit-state checks:

| Backend | Module and property | Bound | Result | Time |
| --- | --- | --- | --- | --- |
| Apalache | `rejected_client_unlock`, `propA_verdict` | 5 steps | counterexample at step 4, none at steps 0 to 3, so 4 steps is the shortest | 16 s |
| Apalache | `chosen`, `safety` | 5 steps | no violation | 38 s |
| Apalache | `chosen`, `safety` | 6 steps | no violation | 116 s |
| Apalache | `chosen`, `safety` | 8 steps | not finished after 25 minutes, stopped (run before the option was added to the file) | 1500 s |
| TLC, breadth first | `chosen`, `safety` | 15 minutes | no violation in 49 163 742 distinct states (409 678 131 generated), search depth 19; 24.5 million states were still queued, so the bounded state space is not exhausted | 900 s |

The TLC command was `quint verify variants.qnt --main=chosen --invariant=safety
--backend=tlc`. Do not start two `quint verify` runs at once: both use one
Apalache server on port 8822, and the second run failed here when the first
one ended. Apalache writes `_apalache-out/` into the working directory; git
ignores it and it can be deleted after a run. TLC keeps its states in a
`quint-tlc-*` directory under `$TMPDIR`; it held 7 GB after the 15 minutes and
is not removed automatically.

## Findings

The numbers refer to the runs recorded above.

### 1. A sleep acknowledgement does not keep the session locked until the sleep

`strongS_ackedSleepCovered` fails on the chosen design. A short trace
(`knownGapUnlockBetweenAckAndSuspendTest`, 17 steps to the suspend):

1. logind announces sleep; the bridge asks the compositor to lock.
2. The compositor locks and presents the lock frame. An unlock attempt is
   opened, the right secret is typed, and the positive verdict is in flight.
3. The compositor acknowledges the lock request; the bridge releases the
   delay inhibitor.
4. The verdict arrives. It is valid for the current epoch and attempt, so the
   session unlocks and a session frame is presented.
5. logind suspends. The panel shows the session.

No invariant in `safety` is violated, and on resume the compositor relocks
before it draws anything (`propB_resume`). What is exposed is the last
session frame, from the resume until the first lock frame is presented. The
window between step 3 and step 5 is short when no other delay inhibitor is
held and can be as long as `InhibitDelayMaxSec` otherwise. The upstream
default is 5 s; the drop-in installed with the current lock raises it to 30 s
(read on the development machine on 2026-11-27 with `busctl get-property
org.freedesktop.login1 /org/freedesktop/login1 org.freedesktop.login1.Manager
InhibitDelayMaxUSec`).

Candidate rule, modelled as `option_sleep_hold`: when the compositor
acknowledges a lock request it closes the open attempt and neither opens an
attempt nor accepts a verdict until the sleep is over. The hold ends on the
clock jump of the resume, or when the bridge reports that logind is not
preparing for sleep.

The first version of this rule kept the hold in compositor memory only. The
simulator refuted it after 20 237 traces: the compositor restarts between
its acknowledgement and the suspend, the new instance knows of no sleep, and
a valid unlock follows (`sleepHoldSurvivesRestart` is that trace). In the
amended version a new instance starts in the hold and leaves it only when the
bridge reports. That version passed the runs recorded above and both progress
tests.

The costs: one more state in the lock reducer, one more bridge message, and a
login or unlock that waits for the bridge after every compositor start. A
bridge that never reports would keep the session locked, so the bridge gains
the power to deny unlock, though still not to grant it. The
rule is not part of the chosen design; it needs a decision.

### 2. Sleep without a lock is reachable and the compositor cannot prevent it

`strongS_sleepCovered` fails in two steps: logind announces sleep and the
inhibitor delay runs out. The same happens when the bridge dies, because its
inhibitor fd closes with it. The design answers with REQ-LOCK-015:
the compositor submits no session frame after the resume. The last session
frame stays on the panel until the first lock frame replaces it.

Two consequences for the implementation:

- The time from `PrepareForSleep` to the presented lock frame has a budget of
  `InhibitDelayMaxSec`. REQ-LOCK-014 keeps the start of `lock/ui`
  out of that budget.
- The bridge is a single point of failure for covered sleep. If the bridge
  passed the inhibitor fd to the compositor, a bridge crash would no longer
  release it, and the compositor would release it by closing the fd when it
  is `Locked`. That would also remove the acknowledgement message and its
  nonce (finding 5). Not modelled; it needs a decision.

### 3. The instance id is the only thing that tells verdicts apart after a restart

Epoch and attempt counters live in compositor memory and restart with it. A
positive verdict for (instance 1, epoch 1, attempt 1) equals the binding of
the first attempt of instance 2 in every field but the instance
(`cx_skip_instance_check`, 15 steps). The instance id therefore has to be a
random value chosen at start, not a counter and not a PID. The same holds for
consent prompts, whose numbers restart as well.

### 4. A resume must void the open attempt, and the clock check must come first

A secret typed before the lid closes produces a verdict that can arrive after
the lid opens again. Without REQ-AUTH-016 it unlocks for
whoever opened the lid (`cx_keep_attempt_on_resume`, 14 steps). The rule only
works if the compositor compares CLOCK_BOOTTIME with CLOCK_MONOTONIC before
it handles any event, including a verdict, and not only before it draws a
frame (`noFrameBeforeJumpGuardTest`, `verdictAcrossSleepGuardTest`). For the
reducer this means that every event carries both clock readings.

### 5. The lock acknowledgement must echo a nonce minted by the bridge

logind's `PrepareForSleep` carries no cycle id. If the bridge releases its
inhibitor on any lock acknowledgement, the acknowledgement of the previous
sleep cycle releases the next one while the session is unlocked
(`cx_stale_ack`, 21 steps).

### 6. "Fresh process per prompt" must include the credential channel

With one long-lived channel from `lock/ui` to `authd`, a secret typed for an
attempt that timed out is judged for the next attempt
(`cx_shared_ui_channel`, 12 steps). `propA_verdict` still holds in that trace;
only the end-to-end property `propA_typed` catches it. The challenge has to
name the channel of the attempt, and `authd` has to read from that one only.

### 7. Revocation in the reducer does not stop an effect that was already emitted

A delivery effect emitted before a lock, a reconnect or a deadline is executed
after it unless the adapter checks the grant again when it runs the effect
(`cx_check_at_enqueue_only`, `cx_grant_by_name`, `cx_timer_only_expiry`,
6 steps each). Clearing queues on revocation is not enough for anything that
has left the reducer.

### 8. The lock is acknowledged by the flip of the lock frame, not by the next flip

A session frame queued before the lock request completes after it. A
compositor that acknowledges on the first completed flip while `Locking`
acknowledges with the session on the panel (`cx_ack_any_flip`, 3 steps). The
frame has to be tagged at submit and the tag read back at the flip event.

### 9. Two behaviours of the reference code must not be carried over

- The bring-up stage with the unchanged lock client over `ext-session-lock`
  is the rejected design. Property A fails there by construction
  (`cx_client_unlock`, 4 steps), so that stage is not fit for daily use.
- niri gives up a lock in progress when the lock frame fails to render
  (`src/niri.rs:5145` at v26.04-324-g02ee0585). In this model that is
  `unlock_on_render_error`, refuted in 2 steps. A file ported from niri that
  contains this branch brings the behaviour with it.

### 10. The checks cover only the properties that are stated

The first revision passed 170 000 traces, Apalache to 6 steps and 49 million
TLC states while it accepted a verdict issued before `authd` disconnected,
addressed Stop to the holder instead of to a stream generation, and measured
the resume rule at submission instead of presentation. A review found all three by reading;
no run could have, because no invariant named them. Each is now an invariant
with a variant that breaks it (`propA_broker`, `propC_stopExact`, the
presentation half of `propB_resume`). New requirements should arrive with
their invariant, not only with a rule in an action.

## From model actions to reducer events

The Rust reducers do not exist yet. The names on the right are proposals; the
point of the table is that every model action has exactly one counterpart, so
a model trace can be replayed as a reducer test.

| Model action | Reducer | Event (input) | Effects (output) |
| --- | --- | --- | --- |
| `compLock` | compositor lock | `LockRequested` | `RevokeAllGrants`, `ScheduleFrame` |
| `compRender(true)` | compositor lock | `FrameSubmitted { content }` | none |
| `compRender(false)` | compositor lock | `FrameFailed` | `ScheduleFrame` |
| `kmsFlipDone` | compositor lock | `FlipCompleted { content }` | `LockAcknowledged { epoch }` when it carried the lock frame |
| `kmsFlipFail` | compositor lock | `FlipFailed` | `ScheduleFrame` |
| `compReadStaleFlip` | compositor lock | `FlipCompleted` for a frame submitted before the last clock jump | `ScheduleFrame`; no acknowledgement |
| `outputOff`, `outputBack` | compositor lock | `OutputDisabled`, `OutputEnabled` | `ScheduleFrame` on enable |
| `compAckNoOutput` | compositor lock | `LockRequested` or `OutputDisabled` while `Locking` with no output | `LockAcknowledged { epoch }` |
| `compOpenAttempt` | compositor lock | `PromptRequested` | `SpawnLockUi { bind }`, `SendChallenge { bind }` |
| `compCloseAttempt` | compositor lock | `AttemptTimedOut`, `LockUiExited` | `KillLockUi` |
| `compRecvVerdict` | compositor lock | `VerdictReceived { bind, ok }` | `Unlock` or `KillLockUi`; a verdict that does not match is dropped |
| `uiUnlockAndDestroy` | none | no such message in `protocol/` | none |
| `logindUnlockSignal` | bridge | `LogindUnlock` | none |
| `compSeeJump` | every compositor reducer | prelude of every event: `boottime - monotonic` grew | `RevokeAllGrants`, `KillLockUi`, `ScheduleFrame` |
| `compRestart` | compositor lock | initial state is `Locking` with a new random instance id | `ScheduleFrame` |
| `userType` | `lock/ui` | key input | credential frame on the attempt's channel |
| `uiCrash` | compositor lock | `LockUiExited` | cover drawn by the compositor |
| `authdRecvChallenge` | `authd` | `ChallengeReceived { bind, channel }` | none |
| `authdRecvCred` | `authd` | `CredentialReceived { channel }` | `StartPwcheck { bind }` |
| `pwcheckRun` | `pwcheck` | request frame | exit status |
| `authdRecvPam` | `authd` | `PwcheckFinished { bind, ok }` | `SendVerdict { bind, ok }` |
| `authdRestart` | `authd`; compositor lock | `authd`: initial state has no challenge. Compositor: `AuthdDisconnected` | `KillLockUi`; the open attempt is consumed |
| `logindPrepareSleep`, `bridgeSeeSleep` | bridge | `PrepareForSleep(true)` | `SendLockRequest { nonce }` |
| `compAckLockReq` | compositor lock | `LockRequestReceived { nonce }`, answered on entering or while `Locked` | `SendLockAck { nonce }` |
| `bridgeRecvAck` | bridge | `LockAckReceived { nonce }` | `ReleaseInhibitor` when the nonce is the current one |
| `compHoldRelease` (option) | compositor lock | `SleepOver` from the bridge | none |
| `logindSuspend(drain)`, `inhibitTimeout(drain)`, `resume` | simulator | faults of the environment; `drain` says whether a pending flip was shown or dropped | none |
| `bridgeCrash`, `bridgeStart` | bridge | process exit; initial state takes an inhibitor | none |
| `compAsk` | compositor grants | `ConsentRequested { holder, window, connection }`; refused for agent capture in v1 | `SpawnConsentUi { ask }` |
| `consentAnswer` | consent UI | the user's choice | answer frame |
| `compRecvAnswer` | compositor grants | `ConsentAnswered { ask, allow }` | `GrantCreated { holder, generation }` |
| `userPressStop`, `compRecvStop` | compositor grants | `StopPressed { holder, generation }` | `CloseStream` when the generation is the live one, else dropped |
| `windowReplaced` | compositor grants | `WindowGone { window, generation }` | `CloseStream`, `KillConsentUi` |
| `deadlinePasses`, `compExpire` | compositor grants | time in the event; `GrantTimerFired` | `CloseStream` |
| `holderRestart` | compositor grants | `ClientDisconnected { connection }` | none |
| `compEnqueue` | compositor grants | a frame or transfer is ready | `Deliver { holder, generation, seq }` |
| `compDispatch` | adapter | executes `Deliver` | checks `grant_live(holder, generation, now)` first, drops otherwise |

Replaying a trace:

1. Produce it: `quint run ... --mbt --out-itf=trace.itf.json`, or take a
   `cx_<defect>` run from `lock.qnt`, which is already a list of actions.
2. Each state in the ITF file carries `mbt::actionTaken` and
   `mbt::nondetPicks`. Map the pair through the table above to one event.
3. Feed the events to the reducers in the simulator, with the model's
   environment steps (`resume`, `kmsFlipDone`, `inhibitTimeout`) as simulator
   faults, and assert the property the trace violated in the model.
4. Name the test after the requirement, for example
   `req_auth_001_stale_instance`.

The 21 `cx_` runs and the 31 tests are the first set to port. A reader for
ITF traces would be a new dependency and needs the usual justification; the
`cx_` runs can be ported by hand without it.

## Requirement ids

The model cites the numeric ids of `docs/requirements.md`. Each id below is
named in a comment next to the action, invariant or test that covers it.

| Catalog id | Where the model covers it |
| --- | --- |
| REQ-LOCK-002 | `kmsFlipDone`, `propB_covered`, `propB_phase`, `staleFlipDoesNotAckTest` |
| REQ-LOCK-007 | `lockNow`, `compLock`, `staleEpochVerdictGuardTest` |
| REQ-LOCK-008 | `uiUnlockAndDestroy`, `uiCannotUnlockGuardTest` |
| REQ-LOCK-010 | `compAckNoOutput` |
| REQ-LOCK-011 | `compRender(false)`, `kmsFlipFail`, `renderErrorStaysLockingGuardTest` |
| REQ-LOCK-013 | `compRestart` (the model starts the new instance covered; the catalog ends the session) |
| REQ-LOCK-014 | `compRender`: the lock frame does not depend on `lock/ui` |
| REQ-LOCK-015, REQ-LOCK-016 | `compSeeJump`, `compReadStaleFlip`, `propB_resume`, `resumeRelocksTest`, `staleFlipAfterResumeGuardTest`, `firstPresentationAfterResumeTest`, `firstPresentationAfterResume2Test` |
| REQ-LOCK-017 | `logindUnlockSignal`, `logindUnlockIgnoredGuardTest` |
| REQ-LOCK-018 | `bridgeRecvAck`, `propS_sleep`, `sleepHandshakeTest` |
| REQ-LOCK-021 | `lockNow`, `compDispatch`, `propC_lockRevokes`, `queuedDeliveryDroppedByLockTest` |
| REQ-LOCK-024 | `compAckLockReq`, `bridgeRecvAck`, `staleLockAckGuardTest` |
| REQ-LOCK-025 | `clockOk`, `noFrameBeforeJumpGuardTest` |
| REQ-AUTH-001 | `compRecvVerdict`, `propA_verdict`, `happyPathTest`, `wrongSecretStaysLockedTest`, `staleInstanceVerdictGuardTest`, `staleEpochVerdictGuardTest` |
| REQ-AUTH-003 | `authdRecvPam`, `propA_pam` |
| REQ-AUTH-004 | `compCloseAttempt`, `lateVerdictAfterTimeoutGuardTest`, `propA_broker` |
| REQ-AUTH-015 | `compRestart`, `staleInstanceVerdictGuardTest` |
| REQ-AUTH-016 | `compSeeJump`, `propA_wake`, `verdictAcrossSleepGuardTest` |
| REQ-AUTH-017 | `authdRestart`, `propA_broker`, `verdictAfterBrokerDisconnectGuardTest` |
| REQ-AUTH-018 | `authdRecvCred`, `propA_typed`, `staleCredentialGuardTest` |
| REQ-TRUST-002 | `compOpenAttempt` |
| REQ-GRANT-005 | `revokeAll` |
| REQ-GRANT-006 | `compSeeJump`, `compRestart`, `compExpire`, `holderRestart` |
| REQ-GRANT-007 | `windowReplaced`, `replacedWindowDropsDeliveryGuardTest`, `consentForReplacedWindowGuardTest` |
| REQ-GRANT-008 | `compRecvStop`, `propC_stopExact`, `staleStopGuardTest`, `stopEndsStreamTest` |
| REQ-GRANT-013 | `compDispatch`, `propC_noLateDelivery`, `expiredDeliveryDroppedGuardTest`, `queuedDeliveryDroppedByLockTest` |
| REQ-GRANT-014 | `compDispatch`, `reconnectDropsDeliveryGuardTest` |
| REQ-GRANT-015 | `compRecvAnswer`, `propC_consent` |
| REQ-AGENT-001 | `compAsk`, `compDispatch`, `propD_agent`, `propD_v1`, `agentCaptureGateTest` |
| REQ-AGENT-005 | `compDispatch`, `agentOneShotTest` |
| REQ-CAP-011 | `agentCaptureGateTest` in `post_v1_agent_capture` |

Proposed, with no catalog id yet. They carry no `REQ-` prefix in the model:

| Name | Statement | State |
| --- | --- | --- |
| `lockAckProgress` | Without further faults a lock request is acknowledged in a bounded number of steps. | proposed; checked by `lockAckProgressTest` |
| `unlockProgress` | Without further faults the right secret unlocks in a bounded number of steps. | proposed; checked by `unlockProgressTest` |
| `sleepHold` | After acknowledging a lock request for a sleep the compositor accepts no verdict until the sleep is over, and a new instance starts in that hold. | proposed; candidate rule of finding 1, modelled as `option_sleep_hold` |

## Limits

- Simulation samples traces; it does not prove the invariants. The Apalache
  runs are bounded to 6 steps, the TLC run did not exhaust the state space,
  and both are from the revision before the review. The longest hand-written counterexample has 21 steps, deeper than
  either checker reached with a guarantee.
- Liveness is a bounded progress check under a fault-free schedule, not a
  proof under fairness. A message in the model stays deliverable forever, so
  the progress tests assume that a sender repeats its pending request when
  the peer reconnects: the compositor its challenge to a restarted `authd`,
  the bridge its lock request to a restarted compositor.
- The stale-flip rule rests on an assumption about the kernel (see
  [Scope](#scope)). If a driver can complete a flip from before the sleep
  after the resume, REQ-LOCK-015 needs a test on that hardware with a frame
  checksum, and the compositor cannot fix it alone.
- After a compositor crash the model sets the panel to blank. That holds when
  the kernel restores the console once the DRM master is gone and the greeter
  takes over. It was not checked on the target machines.
- The model checks ordering. It says nothing about memory, parsing, timing in
  milliseconds, or what a frame contains.
- A step of the model is atomic. A reducer that handles one event per
  transition matches it; anything that splits a model step across two events
  needs a new look at the model.
