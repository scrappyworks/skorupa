# Cross-process ordering model

A [Quint](https://quint.sh) model of the order of events between the
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

- The model exercises 15 safety predicates. The recorded runs below distinguish
  random sampling, bounded symbolic checking and explicit-state search.
- All 20 variants are refuted, each by a random simulation and by a short
  hand-written trace. The rejected `ext-session-lock` design is refuted in
  4 steps, and Apalache confirms that no shorter trace exists.
- The chosen design does **not** guarantee that the machine only sleeps with
  the panel covered, even after it acknowledged the lock for that sleep.
  A candidate rule for the acknowledged case is modelled. Its first version
  was refuted by the simulator; the amended one passed 400 000 traces.
  See [Findings](#findings).

## How to run

Run from the repository root with Nix flakes enabled. A per-command opt-in
is `nix --extra-experimental-features 'nix-command flakes' develop`. For a
single command with flakes already enabled, use `nix develop -c quint typecheck spec/variants.qnt`.

```sh
nix develop
cd spec
```

The pinned nixpkgs package supplies Quint 0.33.1, Rust evaluator 0.7.1,
Apalache 0.62.4 and an OpenJDK 21 runtime. The witness flags below work on
that pin. Quint 0.34 introduced `observation` declarations and deprecated
`--witnesses`. At upgrade, declare the positive witness predicates as
observations in a coverage module importing the appropriate instance. Keep
the capture observation in the post-v1 instance. See the
[0.34 release](https://github.com/quint-co/quint/releases/tag/v0.34.0).

```sh
# Types and names.
quint typecheck variants.qnt

# Scenario, guard and progress tests on the chosen design (32 tests).
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
    witStaleFlip witStop witTransferGrant witCaptureGrant reachInst reachEpoch reachAttempt \
    reachAsk reachGrant reachJob reachWin reachConn reachSleep

# Fault storm: each group occurs once in the scheduler.
quint run variants.qnt --main=chosen --step=stepStorm --invariant=safety \
  --max-samples=50000 --max-steps=100 --n-threads=4

# The rejected design violates safety. Its fixed test identifies propA_verdict.
quint run variants.qnt --main=rejected_client_unlock --invariant=safety

# Its hand-written counterexample as a test. Every variant has one.
quint test --main=rejected_client_unlock --match=counterexample variants.qnt

# Bounded symbolic check with Apalache (heavy: run it through the job queue).
quint verify variants.qnt --main=chosen --invariant=safety --max-steps=6

# Explicit-state search with TLC (limit wall time and disk for this open-ended search).
timeout -k 10s 150s quint verify variants.qnt --main=chosen --invariant=safety --backend=tlc
```

Record the model revision, CLI and evaluator versions, backend, init, step,
bounds and flags. In the pinned Rust evaluator a starting seed selects a
single worker. The printed replay seed selects one trace. With that seed,
`--max-samples=1 --mbt --out-itf=trace.itf.json` captures it. An ITF file
preserves the states already observed. Comparing replayed states also checks
whether the replay used the intended model.
[Tagged evaluator](https://github.com/quint-co/quint/blob/v0.33.1/evaluator/src/main.rs)

`quint test` selects names ending in `Test` by default. Use
`--match=counterexample` on rejected designs, and check the count. The
0.33.1 CLI exits successfully for an empty match. Run all tests on
`post_v1_agent_capture`, and select `'ProgressTest|sleepHold.*Test'` on the
hold option. For sampled progress tests, a seed alone selects one sample.
Pass `--max-samples=10000` with a seed for the full batch.

## Scope

Processes and what the model keeps of each:

| Process | Modelled | Left out |
| --- | --- | --- |
| compositor | lock phase, epoch, attempt, instance id, consent prompt, grant table; restart loses all of it | layout, input routing, rendering |
| kernel (KMS) | frame on the panel, one pending page flip, flip success and failure, output off and on; a flip pending at suspend is shown before the sleep or dropped, and its event arrives after the resume | modes, planes, more than one output |
| `lock/ui` | one process and one credential channel per attempt; crash; the secret it forwards is right or wrong | drawing, keyboard handling |
| `authd` | current challenge, one credential per selection of that challenge, restart loses both and closes the compositor's open attempt | polkit sessions, rate limiting |
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
- Bounds of `step`, and the share of the 40 000 traces of the earlier
  post-review batch that reached each one:

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

  Those limits apply to the random scheduler. Progress suffixes call the
  underlying actions directly. There is one window and one output. The nonce
  `sl.nextReq` and message history can grow without limit even under `step`.
  During one pending sleep, repeating `bridgeCrash`, `bridgeStart` and
  `bridgeSeeSleep` mints another nonce each time. Depth-limited checks still
  have finite search trees. Ordinary TLC breadth-first search on this
  instance cannot terminate by exhausting the reachable states.

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
| B | `propB_resume` | after a resume no session frame is submitted and none is presented before an unlock by verdict in the same wake period (the unconditional first-submission requirement is stronger) | REQ-LOCK-015, REQ-LOCK-016, REQ-LOCK-025 |
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

Witnesses (`wit*`) exercise positive paths: a verified
unlock, an acknowledgement, a grant, a delivery, a dropped delivery, a sleep
released by acknowledgement, a negative verdict, a second epoch, a second
compositor instance and an unlock after a resume are all reached.

Two stronger statements are in the model but outside `safety`, because the
chosen design does not meet them: `strongS_sleepCovered` and
`strongS_ackedSleepCovered`.

## Variants

Each variant removes one rule. "Steps" is the length of the original fixed
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

Three additional scenarios distinguish the received message from other valid
messages already in flight. On `ablate_instance`, a stale verdict breaks
`propA_verdict` at step 20 and a stale Allow breaks `propC_consent` at
step 15. On `ablate_stale_ack` the stale nonce breaks `propS_sleep` at step
24 even though both requests have acknowledgements. The second dispatch
scenario uses an agent and breaks `propD_agent` at step 7.

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

The historical batches are from 2026-11-27 with Quint 0.33.1 from nixpkgs (Rust evaluator
0.7.1, Apalache 0.62.4). The historical runs below cover two revisions. A review changed four
things: an `authd` disconnect closes the open attempt, flips left over from
before a sleep are modelled, Stop names a stream generation, and agent
capture of a host window is an option. Each historical result applies to its
recorded revision.

### Validation of the working tree, 2026-11-27

Base `7489d66`, with the Linguist modelines already removed. Final Quint
files are identified by SHA-256 below. Both batches used the flake's Quint
0.33.1 package. Tests and simulations used its Rust evaluator. Times are wall
time including CLI startup and queue overhead. Seed `0x21` applies to both
columns unless a different seed is shown. The two chosen `step` batches
reuse the seed and overlap.

| Check | Before | After | Wall time before / after |
| --- | --- | --- | --- |
| Typecheck | pass | pass | 2.42 s / 2.47 s |
| Chosen default tests, progress × 10,000 | 31 pass | 32 pass | 33.71 s / 30.69 s |
| Post-v1 default tests, progress × 10,000 | 31 pass | 32 pass | 31.7 s / 32.7 s |
| Hold option, progress × 10,000 and two hold tests | 4 pass | 4 pass | 32.71 s / 30.69 s |
| Fixed counterexamples on twenty ablations | 21 pass | 24 pass | 50.65 s / 53.69 s |
| Chosen negative controls | 21 expected failures | 24 expected failures | 5.16 s / 5.92 s |
| Chosen safety, 100,000 × 100 | no violation | no violation | 409.41 s / 374.33 s |
| 15 individual predicates and 23 witnesses, 100,000 × 100 | no violation | no violation | 471.66 s / 429.73 s |
| Fault storm, 50,000 × 100 | no violation | no violation | 170.49 s / 152.96 s |
| Rejected design, seed 0x21 | violated at step 10 | violated at step 10 | 2.58 s / 2.66 s |
| Post-v1 safety, 10,000 × 100, seed 0x22 | — | no violation | 42.28 s |
| Hold safety and acknowledged coverage, 10,000 × 100, seed 0x23 | — | no violation | 44.3 s |

Chosen witness counts per 100,000 traces, including the deliberately absent v1 capture grant. The grant issuance limit is the least frequently reached bound.

| Predicate | Before | After | Predicate | Before | After |
| --- | ---: | ---: | --- | ---: | ---: |
| `witUnlock` | 34,979 | 34,979 | `witTransferGrant` | 14,417 | 14,417 |
| `witAck` | 100,000 | 100,000 | `witCaptureGrant` | 0 | 0 |
| `witDelivery` | 11,488 | 11,488 | `reachInst` | 72,292 | 72,292 |
| `witSleepByAck` | 66,212 | 66,212 | `reachEpoch` | 3,346 | 3,346 |
| `witNegativeVerdict` | 36,665 | 36,665 | `reachAttempt` | 98,657 | 98,657 |
| `witDrop` | 15,771 | 15,771 | `reachAsk` | 47,640 | 47,640 |
| `witGrant` | 38,954 | 38,954 | `reachGrant` | 369 | 369 |
| `witSecondEpoch` | 29,883 | 29,883 | `reachJob` | 6,231 | 6,231 |
| `witSecondInstance` | 93,618 | 93,618 | `reachWin` | 90,525 | 90,525 |
| `witUnlockAfterResume` | 30,864 | 30,864 | `reachConn` | 92,390 | 92,390 |
| `witStaleFlip` | 47,488 | 47,488 | `reachSleep` | 99,989 | 99,989 |
| `witStop` | 4,071 | 4,071 |  |  |  |

The one-size-up sensitivity run raised all nine scheduler limits by one.
At 1,000 traces of 120 steps (seed `0x41`) all nine new thresholds were
reached. The fourth grant appeared in one trace. Fixed scenarios span
2–24 transitions. A release before suspend, a post-resume unlock, and
two messages with colliding counters need deeper paths than a six-step check.

A 128-trace MBT batch (100 steps, seed `0x77`) recorded every one of the
40 primitive actions enabled in v1. The two rejected unlock paths and the
hold release are variant-only. The hold option exercised its release 133
times in a second batch (seed `0x78`). Positive witness counts measure traces
that contain a state. For example, `witStop` observes a Stop message and
`witSleepByAck` observes inhibitor release. Scenario tests cover consumption
and actual suspend.

Three additional scratch-only mutations tested predicates without a dedicated
named ablation. Issuing a positive verdict for failed PAM violated
`propA_pam`. Entering Locked directly on a lock request violated
`propB_phase`. Opening a capture prompt in v1 violated `propD_v1`. Each
negative-control trace failed on the chosen design and passed on its mutation.

The acknowledged-sleep violation on chosen replayed from `0x15fef` at step
29 with `--invariant=strongS_ackedSleepCovered --mbt --max-steps=100
--max-samples=1 --n-threads=1`. The replay's ITF states matched the search
trace. The fixed scenario reaches suspend in 17 transitions. The timeout case
violates `strongS_sleepCovered` in two. The option held in both the direct
suspend and restart-before-suspend scenarios.

- `lock.qnt` SHA-256 `0ea28b65f44bb12b9712fd52a690f622df88fe76d5d8edf37ccb02225a253290`
- `variants.qnt` SHA-256 `adefad5244656626770625f532066b4d6215d795871cfb3de88a49a97515089c`

Apalache 0.62.4 checked `safety` from the real `init` through six
transitions of the chosen scheduler (exit 0, 189.62 s). This covers all
paths through that depth within the model. The 17-step acknowledged sleep
and 20-step coincident-verdict scenarios illustrate what lies beyond it.

TLC reached search level 15. Its last progress record contained 38,103,953
generated states, 5,128,816 distinct states and 2,861,788 states still
queued. The 150 s wall limit stopped the four-worker run (exit 124,
150.89 s including startup and queue overhead). That progress record
does not establish completion of level 15. Even unlimited time would not
exhaust this instance because a bridge restart loop increments the nonce.

Seven individual Apalache induction attempts checked the base case for
`propA_verdict`, all four C predicates and both D predicates. Preservation
requires a domain assignment for every state variable. A history Boolean or a
condition on the grant map alone did not supply one. The CLI stopped in
transition extraction (exit 1 for each); none of these diagnostics was a
reachable safety violation. The proposed strengthening related positive
verdicts, PAM results and Allows to their histories, fixed the two holder-map
domains, and related effective grants to issuance, window, expiry, use and
revocation. With the nine scheduler limits reduced to at most two, this
conjunction and its eight target predicates passed reachable BMC through five
steps (109.68 s). The direct induction attempt also needed state domains.

Diagnostic one-step checks generated all seven typed records, limited to
three members per child collection. For the eight predicates alone, Apalache
produced a state containing a positive PAM message with no matching PAM
history. Relaying it violated `propA_pam`. With the initial strengthening,
a current consent could still replace a grant with a generation ahead of
the allocator and create a grant for a stale window. The trace started with
allocator 1, old grant 21, prompt window 60 and actual window 22.
These are counterexamples to preservation from synthetic states. Both are
excluded by the transition history from the real initial state.



Adding consistency of the open prompt, grant generations, connection ordering
and the allocator gave a refined conjunction. It passed depth-five BMC from
the real initial state on the smaller v1 and post-v1 instances
(169.60 s and 177.60 s). A smaller
conjunction covering verdict, PAM, consent, lock revocation, exact Stop and
the v1 capture gate also passed a one-step generated-state check
(21.66 s).

The first generated-state check of the refined conjunction failed at state
zero on both variants. The ITFs showed a Portal tag carrying a fresh payload
instead of the model's unit payload. Restricting phases, frames and holders
to the model's actual constructors removed that discrepancy. The resulting
conjunction passed reachable BMC through depth three on both instances
(23.67 s and 23.67 s).
It also passed one full scheduler transition from generated states satisfying
the conjunction in both v1 and post-v1 (44.32 s and 49.82 s).
The eight safety targets include one-shot delivery and the four grant
properties. These are symbolic preservation checks with small scheduler
bounds and at most three members per generated child collection. Establishing
coverage of all reachable collection sizes remains a proof obligation.

The 10,000-trace option batch reached acknowledged release 6,559 times.
The post-v1 batch reached a live agent capture grant 1,465 times. These are
counts of traces with a witness state.

### Verification plan

`chosen` binds `DEFECT = "none"` and `AGENT_CAPTURE = false`. The model
states 15 safety predicates, two stronger sleep predicates and two progress
schedules. There are no temporal formulas or separately specified universal
contracts for pure functions. For the broken designs a finite execution
ending in the negated predicate suffices.

| Property | Reachability evidence | Method and scope |
| --- | --- | --- |
| `propA_verdict` | `happyPathTest` consumes the current success | Induction with positive verdict provenance. Compare both in-flight successes in the instance ablation. |
| `propA_pam` | The same run issues a positive PAM result and verdict | Induction with the relation between positive PAM messages and successful bindings. |
| `propA_typed` | `happyPathTest` and the two-attempt channel scenario | Bounded checking plus the deeper channel test. A proof needs the credential-to-verdict chain. |
| `propA_wake` | `witUnlockAfterResume`, `verdictAcrossSleepGuardTest` | Bounded checking plus the resume trace. Induction needs the open attempt's wake history. |
| `propA_broker` | `verdictAfterBrokerDisconnectGuardTest` has an issued verdict across disconnect | Bounded checking plus the disconnect trace. Induction needs the open attempt's broker lifetime. |
| `propB_covered` | `happyPathTest` sets `mustCover` | Bounded checking and flip ablations. An unbounded proof must relate both pending and visible frames to the lock. |
| `propB_phase` | `happyPathTest` enters `Locked` after a flip | Bounded checking plus output-off and stale-completion paths. |
| `propB_resume` | `resumeRelocksTest` presents a fresh lock frame | Bounded checking and the two-suspend regression. The catalog's first-submission condition is tested separately. |
| `propS_sleep` | `sleepHandshakeTest` reaches both release and suspend | Bounded checking plus the two-nonce ablation. The valid acknowledgement may predate release. |
| `propC_noLateDelivery` | `queuedDeliveryDroppedByLockTest` delivers and subsequently drops | Induction with effective-grant, issued-generation and revocation history. |
| `propC_lockRevokes` | The same run locks with a live grant and a queued job | Induction with the holder map domain, lock phase and prompt state. |
| `propC_consent` | `toGrant` consumes an Allow for the open prompt | Induction with provenance of positive answers. The stale-Allow scenario tests the monitor independently. |
| `propC_stopExact` | `stopEndsStreamTest` consumes a Stop and `staleStopGuardTest` tries an older one | Induction over the accepted Stop's holder and generation. |
| `propD_agent` | `agentOneShotTest` and post-v1 `agentCaptureGateTest` queue two effects | Induction with delivery history, consumption and generation freshness. |
| `propD_v1` | The v1 gate rejects capture at init while transfer succeeds | Induction over prompt, table and queue. The property is constant true after v1, where the one-shot test exercises capture. |
| `strongS_sleepCovered`, `strongS_ackedSleepCovered` | Timeout and acknowledged-sleep traces | Refute each on chosen. For the option, sample the acknowledged predicate and check the hold and restart paths. Universal coverage of sleep would also require a guarantee from logind. |
| `wit*`, `reach*` | Positive state predicates | Count sampled traces. One reaching trace establishes existence. The capture-grant state is intentionally unreachable in v1 and reached after v1. |
| `lockAckProgressTest`, `unlockProgressTest` | Endpoints of sampled 40-step prefixes | Run the deterministic recovery schedule, at most 12 and 20 transitions respectively (32 for unlock with the hold). Fair-schedule liveness would need explicit delivery assumptions. |

For a Lean proof, instantiate the chosen design first. A universal theorem
over an unrestricted `DEFECT` would include the rejected designs. Establish
map domains, message provenance, monotone history and generation freshness,
then prove each affected action preserves them. Use `#gen_steps` and
`reachable_induction` in the Quint/Veil embedding, and check the named
properties with `quint verify --backend veil --proofs` on 0.35 or later.
The kernel-checked route avoids adding trusted SMT answers. The tagged
[0.35 changelog](https://github.com/quint-co/quint/blob/v0.35.0/CHANGELOG.md)
includes Veil induction and proof checking. The live CLI manual's
Apalache-only description of induction predates that release.

### Historical post-review revision

This earlier batch used simulation and scenario tests.

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
| TLC, breadth first | `chosen`, `safety` | 15 minutes | no violation in 49 163 742 distinct states (409 678 131 generated), search depth 19; 24.5 million states were still queued, so the search did not finish | 900 s |

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
held. Its duration depends on logind and the other inhibitor holders. The
model measures transition order only.

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

The costs are one more state in the lock reducer, one more bridge message, and a
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

### 11. Two catalog acceptance criteria exceed the chosen predicates

`quint test variants.qnt --main=chosen --match=catalogGap` exercises both
gaps deterministically. The final states satisfy all 15 predicates.

- **REQ-LOCK-018.** The bridge can receive a valid acknowledgement after an
  authorized unlock and presentation. The panel is already uncovered at
  release, earlier than the known gap between release and suspend.
  `propS_sleep` records where the acknowledgement came from. O-29 would hold
  the lock starting at acknowledgement.
- **REQ-LOCK-015.** If the compositor was already locked across suspend, it
  can open a fresh attempt and unlock after resume before submitting any
  frame. Its first submission can then be a session frame. `propB_resume`
  covers the interval ending at authorized unlock. Decide whether to require a
  fresh presentation before enabling attempts or qualify the catalog wording.

### 12. Which boundary an ordering test observes

- Opening a consent prompt leaves an existing portal grant effective. A
  delivery can occur with the prompt open. The model has no scene or pixels.
  REQ-TRUST-006 and REQ-TRUST-007 need separate tests for suppression of
  capture during trusted input and exclusion of trusted surfaces.
- The holder's generation changes at reconnect. A record can still have
  `live = true` and an old connection until cleanup. Dispatch drops the job.
  This checks the delivery criterion of REQ-GRANT-014. Resource teardown or
  what an indicator displays needs a separate observation.
- `auth.used` belongs to the selected challenge. Replaying an old challenge
  followed by the current one resets it, so both good and bad credentials for
  one binding can enter the check history. The compositor still consumes an
  open attempt once. A claim of one PAM invocation per binding would require
  a transport or deduplication rule.
- `uiCrash` and `compCloseAttempt` are distinct. A current positive verdict
  can arrive after the former and before the latter. Specify the ordering of
  UI-exit notification and verdict processing in the reducer tests. There is
  no consent-UI crash event or bounded restart backoff in this model.


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
   `mbt::nondetPicks`. Use the table and adjacent states to recover action arguments. For example,
   the two literal arguments of `compRender` produce no nondeterministic pick.
3. Feed the events to the reducers in the simulator, with the model's
   environment steps (`resume`, `kmsFlipDone`, `inhibitTimeout`) as simulator
   faults, and assert the property the trace violated in the model.
4. Name the test after the requirement, for example
   `req_auth_001_stale_instance`.

The 24 `cx_` runs and the 32 chosen tests are the first set to port. A reader for
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
  results have explicit depth bounds. TLC did not exhaust the state space. The longest fixed ablation scenario has 24 transitions.
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
