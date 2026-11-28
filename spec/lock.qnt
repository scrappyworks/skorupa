// Cross-process ordering model for lock, unlock, sleep and grants.
//
// Written before any implementation. It is a proposal for the contract in
// `protocol/`, small enough to simulate in seconds. See README.md in this
// directory for scope, abstractions, commands and the mapping to reducer events.
//
// `DEFECT` selects one deliberately broken variant of the design. The chosen
// design uses `NoDefect`. Every other value removes one rule, except
// `EnvResettleFlip`, which weakens one assumption about the kernel.

/// The rule a variant removes. A sum type, so a misspelt name is a type error
/// instead of a silent copy of the chosen design.
module defects {
  type Defect =
    | NoDefect
    // unlock authority
    | ClientUnlock | HonorLogindUnlock | UnlockOnRenderError
    | SkipInstanceCheck | SkipEpochCheck | SkipAttemptCheck | SharedUiChannel
    | KeepAttemptOnResume | KeepAttemptOnAuthdDisconnect | ChallengeReplay
    // presentation
    | AckOnSubmit | AckAnyFlip | NoBoottime | AckStaleFlip
    | KeepLockedOnResume | BlindAttempt
    // sleep
    | StaleAck | NoSleepHold | HoldMemoryOnly | HoldAnyReport | NoFdStore
    // grants
    | NoRevokeOnLock | CheckAtEnqueueOnly | GrantByName | TimerOnlyExpiry | SkipWindowGen
    | StopByHolder | DeliverDuringPrompt
    // environment: a flip the kernel dropped at one suspend may be shown at a later one
    | EnvResettleFlip
}

module lock {
  import defects.*

  const DEFECT: Defect

  /// Capture of one host window by an agent is planned after v1. With `false`
  /// (v1) the compositor refuses every such request and the only agent grant
  /// is the one-shot clipboard transfer.
  const AGENT_CAPTURE: bool

  pure def defect(d: Defect): bool = DEFECT == d

  // ---------------------------------------------------------------- bounds
  // Bounds are applied in `step` only, never inside an action, so the
  // deterministic scheduler used by the progress tests is not cut short.
  pure val MAX_INST = 3      // compositor instances
  pure val MAX_EPOCH = 3     // lock epochs per instance
  pure val MAX_ATTEMPT = 2   // unlock attempts per epoch
  pure val MAX_ASK = 2       // consent prompts per instance
  pure val MAX_GRANT = 3     // grants issued in a whole run
  pure val MAX_WIN = 2       // window generations
  pure val MAX_CONN = 1      // reconnects per grant holder
  pure val MAX_SLEEP = 2     // suspend cycles
  pure val MAX_JOB = 3       // delivery effects emitted in a whole run
  pure val MAX_REQ = 4       // lock requests minted by the bridge in a whole run

  // ----------------------------------------------------------------- types
  type Phase = Unlocked | Locking | Locked
  type Frame = SessionFrame | LockFrame | Blank
  type Holder = Portal | AgentCapture | AgentTransfer

  /// What a verdict is bound to. `inst` stands for the pair (compositor
  /// instance, logind session): a compositor restart ends the session and
  /// returns to the greeter, so the two change together in this design.
  type Bind = { inst: int, epoch: int, attempt: int }
  type Cred = { b: Bind, good: bool }
  type Result = { b: Bind, ok: bool }

  type AskId = { inst: int, n: int }
  type Ask = { open: bool, id: AskId, h: Holder, win: int, conn: int }
  type Answer = { id: AskId, allow: bool }
  type Stop = { h: Holder, gen: int }
  type Grant = { live: bool, gen: int, win: int, conn: int }
  type Job = { h: Holder, gen: int, seq: int }

  /// Names one period of the sleep hold of one compositor instance. `inst` is
  /// the random instance id, so tokens of different instances never collide.
  type Tok = { inst: int, n: int }

  /// Compositor memory. All of it is lost on restart.
  type Comp = {
    inst: int,
    phase: Phase,
    epoch: int,
    attempt: int,
    open: bool,               // an unlock attempt is open (state "Checking")
    hold: bool,               // REQ-LOCK-026: no unlock until the sleep is over
    holdN: int,               // hold periods of this instance; names the current hold token
    ask: Ask,
    askN: int,
    grants: Holder -> Grant,
  }

  /// The panel as the kernel sees it.
  /// `resolved`: the pending flip was settled by the kernel when the machine
  /// went to sleep; only its event is left for the compositor to read.
  type Kms = { screen: Frame, busy: bool, pending: Frame, outputOn: bool, resolved: bool }

  /// logind, the D-Bus bridge in lock/ and the suspend state of the machine.
  type Sleep = {
    pending: bool,            // logind announced sleep and waits for delay inhibitors
    inhibit: bool,            // the bridge holds a delay inhibitor
    suspended: bool,
    jump: bool,               // CLOCK_BOOTTIME ran ahead of CLOCK_MONOTONIC, not yet seen
    bridgeUp: bool,
    req: int,                 // nonce of the lock request the bridge waits on, 0 = none
    nextReq: int,
    cycles: int,
  }

  /// authd memory (lost on restart) and the lock/ui process.
  type Auth = { ch: Bind, used: bool, uiUp: bool, uiFor: Bind }

  /// Facts outside every modelled process.
  type World = {
    win: int,
    conn: Holder -> int,
    nextGen: int,
    expired: Set[int],        // grants whose deadline has passed in boot time
    queue: Set[Job],          // effects the reducer emitted, not yet executed
    nextJob: int,
  }

  /// Every message ever sent. A message stays deliverable forever, so one set
  /// per kind covers loss (never delivered), duplication and reordering, and
  /// also stale messages that outlive a restart of either end.
  type Net = {
    challenges: Set[Bind],    // compositor -> authd
    creds: Set[Cred],         // lock/ui -> authd, on the channel of one attempt
    checks: Set[Cred],        // authd -> pwcheck
    pams: Set[Result],        // pwcheck -> authd
    verdicts: Set[Result],    // authd -> compositor
    lockReqs: Set[int],       // bridge -> compositor
    lockAcks: Set[int],       // compositor -> bridge
    holdQs: Set[Tok],         // compositor -> bridge: hold token, sent with an ack and at start
    sleepOvers: Set[Tok],     // bridge -> compositor: logind is not preparing for sleep
    asks: Set[AskId],         // compositor -> consent UI
    answers: Set[Answer],     // consent UI -> compositor
    stops: Set[Stop],         // indicator -> compositor
  }

  /// History used by the monitors. The environment also uses issued grant IDs
  /// to choose deadlines.
  type Ghost = {
    pamOk: Set[Bind],         // PAM succeeded for this binding
    okIssued: Set[Bind],      // authd issued a positive verdict for this binding
    typedGood: Set[Bind],     // the user typed the right secret into the UI of this attempt
    badUnlock: bool,          // A
    untypedUnlock: bool,      // A, end to end
    staleWakeUnlock: bool,    // A across suspend
    deadBroker: Set[Bind],    // verdicts issued by an authd that has since gone
    brokerUnlock: bool,       // A across an authd disconnect
    mustCover: bool,          // B: acknowledged and not yet unlocked by a verdict
    openedThisWake: bool,
    mustRelock: bool,         // resumed and not yet unlocked by a verdict since
    sessionAfterResume: bool, // B across suspend: submitted or presented
    lockedSinceReq: bool,
    badRelease: bool,         // sleep released without a lock for this request
    ackSleep: bool,           // this sleep cycle was released by a valid acknowledgement
    cycleReqs: Set[int],      // lock requests the bridge minted in the pending sleep cycle
    committed: bool,          // the compositor acknowledged a request of the pending cycle
    uncoveredRelease: bool,   // released by a valid acknowledgement with the session unlocked or visible
    crashRelease: bool,       // a bridge crash alone released the delay of a pending sleep
    nextMustBeLock: bool,     // B: the next submitted frame must be a lock frame
    sessionFirst: bool,       // B: it was a session frame
    promptDelivery: bool,     // trusted mode: delivery while a prompt was open or the session locked
    checked: Set[Bind],       // bindings this authd instance has sent to pwcheck
    doubleCheck: bool,        // A: two credential checks for one binding by one authd instance
    issued: Set[int],
    revoked: Set[int],
    allowed: Set[AskId],
    unconsented: bool,        // C: grant without the user's allow
    wrongStop: bool,          // C: Stop ended another stream than the one meant
    late: Set[Holder],        // C, D: delivery after revocation
    used: Set[int],
    reuse: bool,              // D: one-shot grant delivered twice
    sawUnlock: bool,          // witnesses
    sawAck: bool,
    sawDelivery: bool,
    sawSleepByAck: bool,
    sawNegativeVerdict: bool,
    sawUnlockAfterResume: bool,
    sawDrop: bool,
    sawHoldReport: bool,
    sawSleepFailed: bool,
    sawSuppressed: bool,
  }

  pure val HOLDERS = Set(Portal, AgentCapture, AgentTransfer)
  pure val NO_BIND: Bind = { inst: 0, epoch: 0, attempt: 0 }
  pure val NO_ASK: Ask = { open: false, id: { inst: 0, n: 0 }, h: Portal, win: 0, conn: 0 }
  pure val DEAD: Grant = { live: false, gen: 0, win: 0, conn: 0 }

  pure def isAgent(h: Holder): bool = h != Portal

  var comp: Comp
  var kms: Kms
  var sl: Sleep
  var auth: Auth
  var world: World
  var net: Net
  var gh: Ghost

  // ------------------------------------------------------ pure reducer parts
  pure def bindOf(c: Comp): Bind = { inst: c.inst, epoch: c.epoch, attempt: c.attempt }

  /// The token of the current hold period.
  pure def tokOf(c: Comp): Tok = { inst: c.inst, n: c.holdN }

  /// Order of bindings as the compositor mints them. `inst` stands for the
  /// order of compositor connections as authd sees them.
  pure def bindLt(a: Bind, b: Bind): bool = or {
    a.inst < b.inst,
    a.inst == b.inst and a.epoch < b.epoch,
    a.inst == b.inst and a.epoch == b.epoch and a.attempt < b.attempt,
  }

  /// REQ-LOCK-026: the hold exists unless a variant removes it.
  pure val HOLD_RULE = not(DEFECT == NoSleepHold)

  /// REQ-GRANT-005: revocation closes the prompt and every grant
  /// in one transition. Effects already emitted are out of the reducer's
  /// reach; `compDispatch` stops those.
  pure def revokeAll(c: Comp): Comp =
    { ...c, ask: NO_ASK, grants: HOLDERS.mapBy(_ => DEAD) }

  /// REQ-LOCK-007, REQ-LOCK-021: a lock request opens a new epoch
  /// and is a revocation. A request while already locking or locked changes nothing.
  pure def lockNow(c: Comp): Comp =
    if (c.phase != Unlocked) c
    else {
      val l = { ...c, phase: Locking, epoch: c.epoch + 1, attempt: 0, open: false }
      if (defect(NoRevokeOnLock)) l else revokeAll(l)
    }

  /// REQ-LOCK-026: a new instance does not know whether its predecessor
  /// acknowledged a sleep, so it starts in the hold until the bridge reports.
  pure def freshComp(inst: int): Comp = {
    inst: inst, phase: Locking, epoch: 1, attempt: 0, open: false,
    hold: HOLD_RULE and not(defect(HoldMemoryOnly)), holdN: 0,
    ask: NO_ASK, askN: 0, grants: HOLDERS.mapBy(_ => DEAD),
  }

  /// Ghost bookkeeping for "the lock is acknowledged".
  pure def ghAck(g: Ghost): Ghost = { ...g, mustCover: true, sawAck: true }

  // -------------------------------------------------------- state predicates
  val awake = not(sl.suspended)

  /// REQ-LOCK-025: the compositor compares CLOCK_BOOTTIME with
  /// CLOCK_MONOTONIC before it handles any other event of a loop iteration.
  val clockOk = awake and (not(sl.jump) or defect(NoBoottime))

  // -------------------------------------------------------------------- init
  action init = all {
    comp' = {
      inst: 1, phase: Unlocked, epoch: 0, attempt: 0, open: false, hold: false, holdN: 0,
      ask: NO_ASK, askN: 0, grants: HOLDERS.mapBy(_ => DEAD),
    },
    kms' = { screen: SessionFrame, busy: false, pending: Blank, outputOn: true, resolved: false },
    sl' = {
      pending: false, inhibit: true, suspended: false, jump: false,
      bridgeUp: true, req: 0, nextReq: 0, cycles: 0,
    },
    auth' = { ch: NO_BIND, used: true, uiUp: false, uiFor: NO_BIND },
    world' = {
      win: 1, conn: HOLDERS.mapBy(_ => 0), nextGen: 0, expired: Set(),
      queue: Set(), nextJob: 0,
    },
    net' = {
      challenges: Set(), creds: Set(), checks: Set(), pams: Set(), verdicts: Set(),
      lockReqs: Set(), lockAcks: Set(), holdQs: Set(), sleepOvers: Set(),
      asks: Set(), answers: Set(), stops: Set(),
    },
    gh' = {
      pamOk: Set(), okIssued: Set(), typedGood: Set(),
      badUnlock: false, untypedUnlock: false, staleWakeUnlock: false,
      deadBroker: Set(), brokerUnlock: false, wrongStop: false,
      mustCover: false, openedThisWake: false, mustRelock: false,
      sessionAfterResume: false, lockedSinceReq: false, badRelease: false, ackSleep: false,
      cycleReqs: Set(), committed: false, uncoveredRelease: false, crashRelease: false,
      nextMustBeLock: false, sessionFirst: false, promptDelivery: false,
      checked: Set(), doubleCheck: false,
      issued: Set(), revoked: Set(), allowed: Set(), unconsented: false,
      late: Set(), used: Set(), reuse: false,
      sawUnlock: false, sawAck: false, sawDelivery: false, sawSleepByAck: false,
      sawNegativeVerdict: false, sawUnlockAfterResume: false, sawDrop: false,
      sawHoldReport: false, sawSleepFailed: false, sawSuppressed: false,
    },
  }

  // ------------------------------------------------- compositor: lock, frames

  /// Lock request from any source: shortcut, idle timer, logind Lock, or
  /// PrepareForSleep relayed by the bridge. REQ-LOCK-007.
  action compLock = all {
    clockOk,
    comp.phase == Unlocked,
    comp' = lockNow(comp),
    gh' = { ...gh, revoked: gh.issued },
    kms' = kms, sl' = sl, auth' = auth, world' = world, net' = net,
  }

  /// The compositor composes one frame and submits a page flip. The content
  /// follows the phase alone: outside `Unlocked` it is the lock frame, drawn
  /// from the lock/ui surface or, without one, a solid cover
  /// (REQ-LOCK-014). `ok = false` is a failed render or commit.
  action compRender(ok: bool): bool = {
    val frame = if (comp.phase == Unlocked) SessionFrame else LockFrame
    val early = ok and defect(AckOnSubmit) and comp.phase == Locking
    val giveUp = not(ok) and defect(UnlockOnRenderError) and comp.phase == Locking
    all {
      clockOk,
      kms.outputOn,
      not(kms.busy),
      kms' = if (ok) { ...kms, busy: true, pending: frame } else kms,
      comp' =
        if (early) { ...comp, phase: Locked }
        else if (giveUp) { ...comp, phase: Unlocked }
        else comp,
      gh' = {
        val g1 = if (early) ghAck(gh) else gh
        { ...g1,
          badUnlock: g1.badUnlock or giveUp,
          sessionAfterResume:
            g1.sessionAfterResume or (ok and frame == SessionFrame and g1.mustRelock),
          sessionFirst: g1.sessionFirst or (ok and frame == SessionFrame and g1.nextMustBeLock),
          nextMustBeLock: g1.nextMustBeLock and not(ok) }
      },
      sl' = sl, auth' = auth, world' = world, net' = net,
    }
  }

  /// The page flip completed: the pending frame is on the panel.
  /// REQ-LOCK-002: the lock is acknowledged here and only when
  /// the completed flip carried a lock frame. A session frame queued before
  /// the lock request may still complete; it acknowledges nothing.
  action kmsFlipDone = {
    val ack = comp.phase == Locking and (kms.pending == LockFrame or defect(AckAnyFlip))
    all {
      awake,
      kms.busy,
      not(kms.resolved),
      kms' = { ...kms, busy: false, screen: kms.pending, pending: Blank },
      comp' = if (ack) { ...comp, phase: Locked } else comp,
      gh' = {
        val g1 = if (ack) ghAck(gh) else gh
        { ...g1,
          sessionAfterResume:
            g1.sessionAfterResume or (kms.pending == SessionFrame and g1.mustRelock) }
      },
      sl' = sl, auth' = auth, world' = world, net' = net,
    }
  }

  /// REQ-LOCK-015: the event of a flip that was pending when the
  /// machine went to sleep says nothing about the panel after the resume: the
  /// kernel may have shown the frame before sleeping or dropped it. The
  /// compositor reads the event after the clock jump, acknowledges nothing on
  /// it and submits a new frame. Without the clock check it cannot tell this
  /// event from a fresh one.
  action compReadStaleFlip = {
    val ack = and {
      defect(AckStaleFlip) or defect(NoBoottime),
      comp.phase == Locking,
      kms.pending == LockFrame,
    }
    all {
      clockOk,
      kms.busy,
      kms.resolved,
      kms' = { ...kms, busy: false, pending: Blank, resolved: false },
      comp' = if (ack) { ...comp, phase: Locked } else comp,
      gh' = if (ack) ghAck(gh) else gh,
      sl' = sl, auth' = auth, world' = world, net' = net,
    }
  }

  /// The page flip failed or never completes: the panel keeps the old frame.
  /// REQ-LOCK-011.
  action kmsFlipFail = all {
    awake,
    kms.busy,
    not(kms.resolved),
    kms' = { ...kms, busy: false, pending: Blank },
    comp' = comp, gh' = gh, sl' = sl, auth' = auth, world' = world, net' = net,
  }

  pure val OFF: Kms = { screen: Blank, busy: false, pending: Blank, outputOn: false, resolved: false }

  /// The only output is switched off or disappears. REQ-LOCK-010: when that
  /// happens outside `Unlocked`, the next frame submitted must be a lock frame.
  action outputOff = all {
    awake,
    kms.outputOn,
    kms' = OFF,
    gh' = { ...gh, nextMustBeLock: gh.nextMustBeLock or comp.phase != Unlocked },
    comp' = comp, sl' = sl, auth' = auth, world' = world, net' = net,
  }

  /// REQ-LOCK-011: the lock frame could not be presented within the retry
  /// bound, so the compositor switches the output off itself. Nothing of the
  /// session stays visible, and the lock is acknowledged by `compAckNoOutput`.
  action compBlankOutput = all {
    clockOk,
    comp.phase == Locking,
    kms.outputOn,
    kms' = OFF,
    gh' = { ...gh, nextMustBeLock: true },
    comp' = comp, sl' = sl, auth' = auth, world' = world, net' = net,
  }

  /// The output is enabled again. REQ-LOCK-010: an acknowledgement given
  /// without an output says nothing about an enabled one, so `Locked` falls
  /// back to `Locking` until a lock frame is presented on it.
  action outputBack = all {
    awake,
    not(kms.outputOn),
    kms' = { ...kms, outputOn: true },
    comp' =
      if (comp.phase == Locked and not(defect(BlindAttempt))) { ...comp, phase: Locking }
      else comp,
    gh' = gh, sl' = sl, auth' = auth, world' = world, net' = net,
  }

  /// REQ-LOCK-010: with no enabled output nothing is visible, so the
  /// lock is acknowledged without a frame. The first frame after the output
  /// returns is a lock frame because content follows the phase.
  action compAckNoOutput = all {
    clockOk,
    comp.phase == Locking,
    not(kms.outputOn),
    comp' = { ...comp, phase: Locked },
    gh' = { ...ghAck(gh), nextMustBeLock: true },
    kms' = kms, sl' = sl, auth' = auth, world' = world, net' = net,
  }

  // ----------------------------------------------- compositor: unlock attempts

  /// REQ-LOCK-015, REQ-LOCK-010: the lock frame of this wake period is on the
  /// enabled output. `Locked` alone does not say so: an acknowledgement can
  /// come from a switched-off output.
  val lockShown = comp.phase == Locked and (kms.outputOn or defect(BlindAttempt))

  /// REQ-TRUST-002: each attempt gets a number, a fresh lock/ui process and
  /// a fresh credential channel; the challenge tells authd the binding.
  /// REQ-LOCK-015: an attempt opens only on a presented lock frame.
  /// REQ-LOCK-026: none opens during the sleep hold.
  action compOpenAttempt = {
    val b = { ...bindOf(comp), attempt: comp.attempt + 1 }
    all {
      clockOk,
      lockShown,
      not(comp.open),
      not(comp.hold),
      comp' = { ...comp, attempt: comp.attempt + 1, open: true },
      auth' = { ...auth, uiUp: true, uiFor: b },
      net' = { ...net, challenges: net.challenges.union(Set(b)) },
      gh' = { ...gh, openedThisWake: true },
      kms' = kms, sl' = sl, world' = world,
    }
  }

  /// Attempt timeout, or the UI exited: the attempt is consumed.
  /// REQ-AUTH-004.
  action compCloseAttempt = all {
    clockOk,
    comp.open,
    comp' = { ...comp, open: false },
    auth' = { ...auth, uiUp: false },
    kms' = kms, sl' = sl, world' = world, net' = net, gh' = gh,
  }

  /// REQ-AUTH-001: a verdict is accepted
  /// only for this instance, this epoch and the open attempt, and it consumes
  /// the attempt whether it is positive or negative. Anything else is dropped.
  action compRecvVerdict(v: Result): bool = {
    val instOk = v.b.inst == comp.inst or defect(SkipInstanceCheck)
    val epochOk = v.b.epoch == comp.epoch or defect(SkipEpochCheck)
    val attemptOk =
      (v.b.attempt == comp.attempt and comp.open) or defect(SkipAttemptCheck)
    val cur = bindOf(comp)
    val verified = comp.open and v.b == cur and gh.okIssued.contains(v.b)
    all {
      net.verdicts.contains(v),
      clockOk,
      lockShown,
      not(comp.hold),
      instOk, epochOk, attemptOk,
      comp' = { ...comp, open: false, phase: if (v.ok) Unlocked else Locked },
      auth' = { ...auth, uiUp: false },
      gh' =
        if (v.ok) {
          ...gh,
          badUnlock: gh.badUnlock or not(verified),
          untypedUnlock: gh.untypedUnlock or not(gh.typedGood.contains(cur)),
          staleWakeUnlock: gh.staleWakeUnlock or not(gh.openedThisWake),
          brokerUnlock: gh.brokerUnlock or gh.deadBroker.contains(cur),
          mustCover: if (verified) false else gh.mustCover,
          mustRelock: if (verified and gh.openedThisWake) false else gh.mustRelock,
          sawUnlock: gh.sawUnlock or verified,
          sawUnlockAfterResume:
            gh.sawUnlockAfterResume or (verified and gh.openedThisWake and gh.mustRelock),
        } else { ...gh, sawNegativeVerdict: true },
      kms' = kms, sl' = sl, world' = world, net' = net,
    }
  }

  /// Rejected design: the ext-session-lock client sends `unlock_and_destroy`
  /// and the compositor obeys. In the chosen design lock/ui has no lock
  /// object, so this request does not exist. REQ-LOCK-008.
  action uiUnlockAndDestroy = all {
    defect(ClientUnlock),
    awake,
    comp.phase == Locked,
    comp' = { ...comp, phase: Unlocked, open: false },
    gh' = { ...gh, badUnlock: true },
    kms' = kms, sl' = sl, auth' = auth, world' = world, net' = net,
  }

  /// logind `Unlock`, as requested by `loginctl unlock-session`.
  /// The chosen design ignores it. REQ-LOCK-017.
  action logindUnlockSignal = all {
    defect(HonorLogindUnlock),
    awake,
    comp.phase == Locked,
    comp' = { ...comp, phase: Unlocked, open: false },
    gh' = { ...gh, badUnlock: true },
    kms' = kms, sl' = sl, auth' = auth, world' = world, net' = net,
  }

  /// REQ-LOCK-015, REQ-AUTH-016,
  /// REQ-GRANT-006: a CLOCK_BOOTTIME jump means the machine slept.
  /// The next frame is a lock frame, the open attempt is void, grants are gone.
  /// A presentation from before the sleep no longer counts: the compositor is
  /// `Locking` until a lock frame submitted after the jump is presented.
  /// REQ-LOCK-026: the sleep is over, so the hold ends.
  action compSeeJump = all {
    awake,
    sl.jump,
    not(defect(NoBoottime)),
    sl' = { ...sl, jump: false },
    comp' = {
      val l = lockNow(comp)
      val c = {
        ...revokeAll(l), hold: false,
        phase: if (defect(KeepLockedOnResume)) l.phase else Locking,
      }
      if (defect(KeepAttemptOnResume)) c else { ...c, open: false }
    },
    auth' =
      if (defect(KeepAttemptOnResume)) auth else { ...auth, uiUp: false },
    gh' = { ...gh, revoked: gh.issued },
    kms' = kms, world' = world, net' = net,
  }

  /// Compositor crash and restart. The session dies with it: the new instance
  /// starts covered (greeter), with a new instance id and counters back at
  /// their start values. REQ-LOCK-013, REQ-AUTH-015.
  action compRestart = all {
    awake,
    comp' = freshComp(comp.inst + 1),
    kms' = { ...kms, screen: Blank, busy: false, pending: Blank, resolved: false },
    sl' = { ...sl, jump: false },
    auth' = { ...auth, uiUp: false },
    gh' = { ...gh, revoked: gh.issued },
    world' = { ...world, queue: Set() },
    // REQ-LOCK-026: the new instance asks the bridge with its first hold token.
    net' =
      if (freshComp(comp.inst + 1).hold)
        { ...net, holdQs: net.holdQs.union(Set(tokOf(freshComp(comp.inst + 1)))) }
      else net,
  }

  // ------------------------------------------------------- lock/ui, authd, PAM

  /// The user types a secret into the UI of the current attempt.
  action userType(good: bool): bool = all {
    awake,
    auth.uiUp,
    net' = { ...net, creds: net.creds.union(Set({ b: auth.uiFor, good: good })) },
    gh' = if (good) { ...gh, typedGood: gh.typedGood.union(Set(auth.uiFor)) } else gh,
    comp' = comp, kms' = kms, sl' = sl, auth' = auth, world' = world,
  }

  action uiCrash = all {
    awake,
    auth.uiUp,
    auth' = { ...auth, uiUp: false },
    comp' = comp, kms' = kms, sl' = sl, world' = world, net' = net, gh' = gh,
  }

  /// authd learns the binding of the attempt it may judge next.
  /// REQ-AUTH-019: only a binding later than the one it holds. A replayed
  /// challenge would otherwise make the same binding judgeable a second time.
  action authdRecvChallenge(b: Bind): bool = all {
    net.challenges.contains(b),
    awake,
    if (defect(ChallengeReplay)) b != auth.ch else bindLt(auth.ch, b),
    auth' = { ...auth, ch: b, used: false },
    comp' = comp, kms' = kms, sl' = sl, world' = world, net' = net, gh' = gh,
  }

  /// REQ-AUTH-018: authd takes one credential per challenge, and only
  /// from the channel of that attempt.
  action authdRecvCred(c: Cred): bool = all {
    net.creds.contains(c),
    awake,
    auth.ch != NO_BIND,
    not(auth.used),
    c.b == auth.ch or defect(SharedUiChannel),
    auth' = { ...auth, used: true },
    net' = { ...net, checks: net.checks.union(Set({ b: auth.ch, good: c.good })) },
    gh' = {
      ...gh,
      doubleCheck: gh.doubleCheck or gh.checked.contains(auth.ch),
      checked: gh.checked.union(Set(auth.ch)),
    },
    comp' = comp, kms' = kms, sl' = sl, world' = world,
  }

  /// pwcheck: one `pam_authenticate` + `pam_acct_mgmt` per request. A crash is
  /// the absence of this step.
  action pwcheckRun(k: Cred): bool = all {
    net.checks.contains(k),
    awake,
    net' = { ...net, pams: net.pams.union(Set({ b: k.b, ok: k.good })) },
    gh' = if (k.good) { ...gh, pamOk: gh.pamOk.union(Set(k.b)) } else gh,
    comp' = comp, kms' = kms, sl' = sl, auth' = auth, world' = world,
  }

  /// REQ-AUTH-003: authd relays a PAM result only for the challenge it
  /// consumed. After an authd restart nothing matches and no verdict leaves.
  action authdRecvPam(r: Result): bool = all {
    net.pams.contains(r),
    awake,
    auth.used,
    r.b == auth.ch,
    net' = { ...net, verdicts: net.verdicts.union(Set(r)) },
    gh' = if (r.ok) { ...gh, okIssued: gh.okIssued.union(Set(r.b)) } else gh,
    comp' = comp, kms' = kms, sl' = sl, auth' = auth, world' = world,
  }

  /// authd dies and a new one starts. REQ-AUTH-017: the
  /// compositor reads the end of the old connection in the same step and
  /// closes the open attempt, so no verdict of the old authd is accepted
  /// afterwards. One step is exact for a stream socket: what was read before
  /// the end was sent before the disconnect, and nothing follows the end.
  action authdRestart = {
    val keep = defect(KeepAttemptOnAuthdDisconnect)
    all {
      awake,
      auth.ch != NO_BIND or comp.open,
      auth' = { ...auth, ch: NO_BIND, used: true, uiUp: keep and auth.uiUp },
      comp' = if (keep) comp else { ...comp, open: false },
      gh' = { ...gh, deadBroker: gh.okIssued, checked: Set() },
      kms' = kms, sl' = sl, world' = world, net' = net,
    }
  }

  // ------------------------------------------------- logind, bridge, suspend

  action logindPrepareSleep = all {
    awake,
    not(sl.pending),
    sl' = { ...sl, pending: true, cycles: sl.cycles + 1 },
    gh' = { ...gh, cycleReqs: Set(), committed: false, ackSleep: false },
    comp' = comp, kms' = kms, auth' = auth, world' = world, net' = net,
  }

  /// The sleep job fails or is aborted (a wake-up source fired, a unit
  /// failed): logind reports PrepareForSleep(false) and the machine never
  /// slept, so no clock jump follows.
  action logindSleepFailed = all {
    awake,
    sl.pending,
    sl' = { ...sl, pending: false, req: 0 },
    gh' = { ...gh, cycleReqs: Set(), committed: false, ackSleep: false, sawSleepFailed: true },
    comp' = comp, kms' = kms, auth' = auth, world' = world, net' = net,
  }

  /// The bridge sees PrepareForSleep(true) and asks the compositor to lock,
  /// with a fresh nonce. A lost signal is the absence of this step.
  action bridgeSeeSleep = {
    val r = sl.nextReq + 1
    all {
      awake,
      sl.bridgeUp,
      sl.pending,
      sl.req == 0,
      sl' = { ...sl, req: r, nextReq: r },
      net' = { ...net, lockReqs: net.lockReqs.union(Set(r)) },
      gh' = { ...gh, lockedSinceReq: false, cycleReqs: gh.cycleReqs.union(Set(r)) },
      comp' = comp, kms' = kms, auth' = auth, world' = world,
    }
  }

  /// REQ-LOCK-024: the compositor answers a lock request only
  /// while `Locked`, and echoes the nonce of that request.
  /// REQ-LOCK-026: the acknowledgement is a commitment. In the same step the
  /// compositor closes the open attempt and enters the sleep hold under a new
  /// hold token, which travels with the acknowledgement.
  action compAckLockReq(r: int): bool = {
    val held = { ...comp, hold: true, open: false, holdN: comp.holdN + 1 }
    all {
      net.lockReqs.contains(r),
      not(net.lockAcks.contains(r)),
      clockOk,
      comp.phase == Locked,
      net' = {
        ...net,
        lockAcks: net.lockAcks.union(Set(r)),
        holdQs: if (HOLD_RULE) net.holdQs.union(Set(tokOf(held))) else net.holdQs,
      },
      gh' = {
        ...gh,
        lockedSinceReq: gh.lockedSinceReq or r == sl.req,
        committed: gh.committed or gh.cycleReqs.contains(r),
      },
      comp' = if (HOLD_RULE) held else comp,
      auth' = if (HOLD_RULE) { ...auth, uiUp: false } else auth,
      kms' = kms, sl' = sl, world' = world,
    }
  }

  /// REQ-LOCK-026: the bridge answers a hold token only after it received the
  /// token, only while logind's `PreparingForSleep` is false, and only while
  /// it holds a delay inhibitor again. The report echoes the token.
  action bridgeReportAwake(t: Tok): bool = all {
    net.holdQs.contains(t),
    not(net.sleepOvers.contains(t)),
    awake,
    sl.bridgeUp,
    sl.inhibit,
    not(sl.pending),
    net' = { ...net, sleepOvers: net.sleepOvers.union(Set(t)) },
    comp' = comp, kms' = kms, sl' = sl, auth' = auth, world' = world, gh' = gh,
  }

  /// REQ-LOCK-026: the hold ends on a report that echoes the current hold
  /// token. A report for an earlier token was made before this hold began and
  /// says nothing about the sleep the compositor has acknowledged since.
  /// After a real sleep the clock jump ends the hold first, in `compSeeJump`.
  action compHoldRelease(t: Tok): bool = all {
    net.sleepOvers.contains(t),
    comp.hold,
    clockOk,
    t == tokOf(comp) or defect(HoldAnyReport),
    comp' = { ...comp, hold: false },
    gh' = { ...gh, sawHoldReport: true },
    kms' = kms, sl' = sl, auth' = auth, world' = world, net' = net,
  }

  /// REQ-LOCK-018: the bridge releases the delay inhibitor only on
  /// an acknowledgement of the request it is waiting on.
  action bridgeRecvAck(r: int): bool = all {
    net.lockAcks.contains(r),
    awake,
    sl.bridgeUp,
    sl.pending,
    sl.inhibit,
    sl.req != 0,
    r == sl.req or defect(StaleAck),
    sl' = { ...sl, inhibit: false },
    gh' = {
      ...gh,
      badRelease: gh.badRelease or r != sl.req or not(gh.lockedSinceReq),
      sawSleepByAck: gh.sawSleepByAck or (r == sl.req and gh.lockedSinceReq),
      ackSleep: r == sl.req and gh.lockedSinceReq,
      uncoveredRelease: gh.uncoveredRelease or and {
        r == sl.req, gh.lockedSinceReq,
        comp.phase == Unlocked or kms.screen == SessionFrame,
      },
    },
    comp' = comp, kms' = kms, auth' = auth, world' = world, net' = net,
  }

  /// What the kernel does with a pending page flip when the machine goes to
  /// sleep. Assumption of the model: it settles the flip before sleeping,
  /// either by showing the frame (`drain`) or by dropping it, and the event
  /// reaches the compositor after the resume. No frame submitted before the
  /// sleep reaches the panel after it.
  /// A flip settled at an earlier suspend stays settled: the panel follows the
  /// committed KMS state, and that changes only by a new commit. The variant
  /// `EnvResettleFlip` drops this half of the assumption.
  pure def atSuspend(k: Kms, drain: bool): Kms =
    if (not(k.busy) or (k.resolved and not(defect(EnvResettleFlip)))) k
    else if (drain) { ...k, screen: k.pending, resolved: true }
    else { ...k, resolved: true }

  /// logind suspends once no delay inhibitor is held.
  action logindSuspend(drain: bool): bool = all {
    awake,
    sl.pending,
    not(sl.inhibit),
    sl' = { ...sl, suspended: true },
    kms' = atSuspend(kms, drain),
    comp' = comp, auth' = auth, world' = world, net' = net, gh' = gh,
  }

  /// InhibitDelayMaxSec ran out: logind suspends although the inhibitor is held.
  action inhibitTimeout(drain: bool): bool = all {
    awake,
    sl.pending,
    sl.inhibit,
    sl' = { ...sl, suspended: true },
    kms' = atSuspend(kms, drain),
    comp' = comp, auth' = auth, world' = world, net' = net, gh' = gh,
  }

  /// The bridge dies. REQ-LOCK-027: its inhibitor descriptor is also in the
  /// service manager's descriptor store, so the delay outlives the process and
  /// the restarted bridge gets the descriptor back.
  action bridgeCrash = all {
    awake,
    sl.bridgeUp,
    sl' = { ...sl, bridgeUp: false, inhibit: sl.inhibit and not(defect(NoFdStore)), req: 0 },
    gh' = {
      ...gh,
      crashRelease: gh.crashRelease or (defect(NoFdStore) and sl.inhibit and sl.pending),
    },
    comp' = comp, kms' = kms, auth' = auth, world' = world, net' = net,
  }

  /// A new bridge inherits the stored inhibitor or takes one; one taken after
  /// PrepareForSleep does not delay the sleep already announced (logind
  /// refuses it).
  action bridgeStart = all {
    awake,
    not(sl.bridgeUp),
    sl' = { ...sl, bridgeUp: true, inhibit: sl.inhibit or not(sl.pending), req: 0 },
    comp' = comp, kms' = kms, auth' = auth, world' = world, net' = net, gh' = gh,
  }

  /// The service manager gives the bridge unit up (start limit, unit stopped)
  /// and closes the stored descriptor. The delay is gone.
  action bridgeFdLost = all {
    awake,
    not(sl.bridgeUp),
    sl.inhibit,
    sl' = { ...sl, inhibit: false },
    comp' = comp, kms' = kms, auth' = auth, world' = world, net' = net, gh' = gh,
  }

  /// After a sleep it released, the bridge takes a new delay inhibitor.
  action bridgeRearm = all {
    awake,
    sl.bridgeUp,
    not(sl.pending),
    not(sl.inhibit),
    sl' = { ...sl, inhibit: true },
    comp' = comp, kms' = kms, auth' = auth, world' = world, net' = net, gh' = gh,
  }

  /// Resume. Userspace has not run yet; the panel shows the last frame. An
  /// inhibitor that was released stays released until the bridge takes a new
  /// one (`bridgeRearm`).
  action resume = all {
    sl.suspended,
    sl' = { ...sl, suspended: false, pending: false, jump: true, req: 0 },
    gh' = {
      ...gh, mustRelock: true, openedThisWake: false, revoked: gh.issued, ackSleep: false,
      cycleReqs: Set(), committed: false, nextMustBeLock: true,
    },
    comp' = comp, kms' = kms, auth' = auth, world' = world, net' = net,
  }

  // ------------------------------------------------- consent, grants, capture

  /// A portal or an agent asks for capture or a transfer; the compositor opens
  /// one consent prompt bound to the window generation and the connection.
  action compAsk(h: Holder): bool = {
    val id = { inst: comp.inst, n: comp.askN + 1 }
    all {
      clockOk,
      comp.phase == Unlocked,
      not(comp.ask.open),
      h != AgentCapture or AGENT_CAPTURE,
      comp' = {
        ...comp,
        askN: comp.askN + 1,
        ask: { open: true, id: id, h: h, win: world.win, conn: world.conn.get(h) },
      },
      net' = { ...net, asks: net.asks.union(Set(id)) },
      kms' = kms, sl' = sl, auth' = auth, world' = world, gh' = gh,
    }
  }

  /// The consent UI (fresh process per prompt) reports the user's choice once.
  action consentAnswer(id: AskId, allow: bool): bool = all {
    net.asks.contains(id),
    awake,
    not(net.answers.exists(a => a.id == id)),
    net' = { ...net, answers: net.answers.union(Set({ id: id, allow: allow })) },
    gh' = if (allow) { ...gh, allowed: gh.allowed.union(Set(id)) } else gh,
    comp' = comp, kms' = kms, sl' = sl, auth' = auth, world' = world,
  }

  /// REQ-GRANT-015: an answer creates a grant only for the open
  /// prompt of this instance, while unlocked, and consumes the prompt.
  action compRecvAnswer(a: Answer): bool = {
    val q = comp.ask
    val gen = world.nextGen + 1
    val old = comp.grants.get(q.h)
    all {
      net.answers.contains(a),
      clockOk,
      comp.phase == Unlocked,
      q.open,
      a.id.n == q.id.n,
      a.id.inst == q.id.inst or defect(SkipInstanceCheck),
      comp' = {
        ...comp,
        ask: NO_ASK,
        grants:
          if (a.allow)
            comp.grants.set(q.h, { live: true, gen: gen, win: q.win, conn: q.conn })
          else comp.grants,
      },
      world' = if (a.allow) { ...world, nextGen: gen } else world,
      gh' =
        if (a.allow) {
          ...gh,
          issued: gh.issued.union(Set(gen)),
          // The replaced grant is revoked. A grant for a window or connection
          // that is already gone is revoked from birth.
          revoked: gh.revoked.union(Set(old.gen)).union(
            if (q.win != world.win or q.conn != world.conn.get(q.h)) Set(gen) else Set()),
          unconsented: gh.unconsented or a.id != q.id or not(gh.allowed.contains(q.id)),
        } else gh,
      kms' = kms, sl' = sl, auth' = auth, net' = net,
    }
  }

  /// REQ-TRUST-010, REQ-TRUST-012: the prompt timed out or its process exited
  /// without an answer. The request is denied; an answer that arrives later
  /// finds no open prompt.
  action compAskClose = all {
    clockOk,
    comp.ask.open,
    comp' = { ...comp, ask: NO_ASK },
    kms' = kms, sl' = sl, auth' = auth, world' = world, net' = net, gh' = gh,
  }

  /// The user presses Stop or Forget on the indicator of a live stream. The
  /// request names the stream generation the indicator was drawn for.
  action userPressStop(h: Holder): bool = all {
    awake,
    comp.grants.get(h).live,
    net' = { ...net, stops: net.stops.union(Set({ h: h, gen: comp.grants.get(h).gen })) },
    comp' = comp, kms' = kms, sl' = sl, auth' = auth, world' = world, gh' = gh,
  }

  /// REQ-GRANT-008: Stop ends the stream of exactly that generation. A Stop
  /// for a generation that is gone is dropped; it does not end the stream the
  /// holder was granted later.
  action compRecvStop(t: Stop): bool = {
    val g = comp.grants.get(t.h)
    all {
      net.stops.contains(t),
      clockOk,
      g.live,
      g.gen == t.gen or defect(StopByHolder),
      comp' = { ...comp, grants: comp.grants.set(t.h, DEAD) },
      gh' = {
        ...gh,
        revoked: gh.revoked.union(Set(g.gen)),
        wrongStop: gh.wrongStop or g.gen != t.gen,
      },
      kms' = kms, sl' = sl, auth' = auth, world' = world, net' = net,
    }
  }

  /// The window is closed or replaced. REQ-GRANT-007: grants and
  /// prompts bound to the old generation end with it.
  action windowReplaced = all {
    awake,
    world' = { ...world, win: world.win + 1 },
    comp' = if (defect(SkipWindowGen)) comp else revokeAll(comp),
    gh' = { ...gh, revoked: gh.issued },
    kms' = kms, sl' = sl, auth' = auth, net' = net,
  }

  /// The deadline of a grant passes in real (boot) time.
  action deadlinePasses(g: int): bool = all {
    gh.issued.contains(g),
    not(world.expired.contains(g)),
    awake,
    world' = { ...world, expired: world.expired.union(Set(g)) },
    gh' = { ...gh, revoked: gh.revoked.union(Set(g)) },
    comp' = comp, kms' = kms, sl' = sl, auth' = auth, net' = net,
  }

  /// The expiry timer of the compositor fires.
  action compExpire(h: Holder): bool = all {
    clockOk,
    comp.grants.get(h).live,
    world.expired.contains(comp.grants.get(h).gen),
    comp' = { ...comp, grants: comp.grants.set(h, DEAD) },
    kms' = kms, sl' = sl, auth' = auth, world' = world, net' = net, gh' = gh,
  }

  /// The portal, the agent or its broker dies and a new process connects.
  action holderRestart(h: Holder): bool = all {
    awake,
    world' = { ...world, conn: world.conn.setBy(h, generation => generation + 1) },
    gh' = { ...gh, revoked: gh.revoked.union(Set(comp.grants.get(h).gen)) },
    comp' = comp, kms' = kms, sl' = sl, auth' = auth, net' = net,
  }

  /// REQ-GRANT-014: the compositor handles the end of the holder's connection
  /// and removes the grant record. Until this step the record still says
  /// `live`; `grantLive` and `compDispatch` already treat it as dead.
  action compSeeDisconnect(h: Holder): bool = all {
    clockOk,
    comp.grants.get(h).live,
    comp.grants.get(h).conn != world.conn.get(h),
    comp' = { ...comp, grants: comp.grants.set(h, DEAD) },
    kms' = kms, sl' = sl, auth' = auth, world' = world, net' = net, gh' = gh,
  }

  /// REQ-GRANT-013: the one predicate every reader of a grant uses: the
  /// record, the lock phase, the connection and the deadline at this moment.
  /// The `live` bit of the record alone is not an answer.
  def grantLive(h: Holder): bool = {
    val g = comp.grants.get(h)
    and {
      g.live,
      comp.phase == Unlocked,
      g.conn == world.conn.get(h) or defect(GrantByName),
      not(world.expired.contains(g.gen)) or defect(TimerOnlyExpiry),
    }
  }

  /// REQ-TRUST-006: trusted mode. A consent prompt is open or the session is
  /// not unlocked; nothing is delivered to a grant holder.
  val trustedMode = comp.ask.open or comp.phase != Unlocked

  /// The reducer decides to deliver (a captured frame, a clipboard transfer)
  /// and emits an effect. The grant is checked here.
  action compEnqueue(h: Holder): bool = all {
    clockOk,
    grantLive(h),
    not(comp.ask.open),
    world' = {
      ...world,
      nextJob: world.nextJob + 1,
      queue: world.queue.union(
        Set({ h: h, gen: comp.grants.get(h).gen, seq: world.nextJob + 1 })),
    },
    comp' = comp, kms' = kms, sl' = sl, auth' = auth, net' = net, gh' = gh,
  }

  /// The adapter executes a queued effect. REQ-GRANT-013: the
  /// grant is checked again here, at the last point the compositor controls:
  /// generation, connection, deadline read from the clock, and lock phase
  /// (REQ-LOCK-021). An invalid effect is dropped.
  /// REQ-AGENT-001, REQ-AGENT-005: delivery to an agent needs a
  /// live grant and consumes it. A delivery dropped for an open prompt
  /// consumes nothing.
  action compDispatch(j: Job): bool = {
    val g = comp.grants.get(j.h)
    val valid = defect(CheckAtEnqueueOnly) or and {
      grantLive(j.h),
      g.gen == j.gen,
      // REQ-TRUST-006: nothing leaves while a trusted prompt is open.
      not(comp.ask.open) or defect(DeliverDuringPrompt),
    }
    val oneShot = valid and isAgent(j.h)
    all {
      world.queue.contains(j),
      clockOk,
      world' = { ...world, queue: world.queue.exclude(Set(j)) },
      comp' =
        if (oneShot and g.gen == j.gen) { ...comp, grants: comp.grants.set(j.h, DEAD) }
        else comp,
      gh' =
        if (valid) {
          ...gh,
          late:
            if (gh.revoked.contains(j.gen) or not(gh.issued.contains(j.gen)))
              gh.late.union(Set(j.h))
            else gh.late,
          reuse: gh.reuse or (isAgent(j.h) and gh.used.contains(j.gen)),
          used: gh.used.union(Set(j.gen)),
          revoked: if (isAgent(j.h)) gh.revoked.union(Set(j.gen)) else gh.revoked,
          promptDelivery: gh.promptDelivery or trustedMode,
          sawDelivery: true,
        } else {
          ...gh, sawDrop: true,
          sawSuppressed: gh.sawSuppressed or (grantLive(j.h) and g.gen == j.gen),
        },
      kms' = kms, sl' = sl, auth' = auth, net' = net,
    }
  }

  // -------------------------------------------------------------------- step
  // Bounds live here, not in the actions. The groups separate the plain path
  // (each receiver takes the message meant for its current state) from stale
  // deliveries and from faults, so that `step` can weight them.

  action stepLock = any {
    compRender(true),
    kmsFlipDone,
    compAckNoOutput,
    all { comp.attempt < MAX_ATTEMPT, compOpenAttempt },
    nondet good = oneOf(Set(true, false))
    userType(good),
    authdRecvChallenge(bindOf(comp)),
    nondet good = oneOf(Set(true, false))
    authdRecvCred({ b: auth.ch, good: good }),
    nondet good = oneOf(Set(true, false))
    all {
      not(net.pams.contains({ b: auth.ch, ok: good })),
      pwcheckRun({ b: auth.ch, good: good }),
    },
    nondet ok = oneOf(Set(true, false))
    all {
      not(net.verdicts.contains({ b: auth.ch, ok: ok })),
      authdRecvPam({ b: auth.ch, ok: ok }),
    },
    nondet ok = oneOf(Set(true, false))
    compRecvVerdict({ b: bindOf(comp), ok: ok }),
    uiUnlockAndDestroy,
    logindUnlockSignal,
  }

  action stepSleep = any {
    all { sl.nextReq < MAX_REQ, bridgeSeeSleep },
    compAckLockReq(sl.req),
    bridgeRecvAck(sl.req),
    nondet drain = oneOf(Set(true, false))
    logindSuspend(drain),
    resume,
    compSeeJump,
    compReadStaleFlip,
    bridgeRearm,
    bridgeReportAwake(tokOf(comp)),
    compHoldRelease(tokOf(comp)),
  }

  action stepGrant = any {
    nondet h = oneOf(HOLDERS)
    all { comp.askN < MAX_ASK, world.nextGen < MAX_GRANT, compAsk(h) },
    nondet allow = oneOf(Set(true, false))
    consentAnswer(comp.ask.id, allow),
    nondet allow = oneOf(Set(true, false))
    compRecvAnswer({ id: comp.ask.id, allow: allow }),
    nondet h = oneOf(HOLDERS)
    all { world.nextJob < MAX_JOB, compEnqueue(h) },
    nondet j = oneOf(world.queue)
    compDispatch(j),
    nondet h = oneOf(HOLDERS)
    compExpire(h),
    nondet h = oneOf(HOLDERS)
    compRecvStop({ h: h, gen: comp.grants.get(h).gen }),
    nondet h = oneOf(HOLDERS)
    compSeeDisconnect(h),
  }

  /// Requests that start a lock or a sleep cycle.
  action stepStart = any {
    all { comp.epoch < MAX_EPOCH, compLock },
    all { sl.cycles < MAX_SLEEP, logindPrepareSleep },
  }

  /// Any message ever sent, delivered again: duplication, reordering and
  /// messages that outlived a restart.
  action stepStale = any {
    nondet b = oneOf(net.challenges)
    authdRecvChallenge(b),
    nondet c = oneOf(net.creds)
    authdRecvCred(c),
    nondet k = oneOf(net.checks)
    all { not(net.pams.contains({ b: k.b, ok: k.good })), pwcheckRun(k) },
    nondet r = oneOf(net.pams)
    all { not(net.verdicts.contains(r)), authdRecvPam(r) },
    nondet v = oneOf(net.verdicts)
    compRecvVerdict(v),
    nondet r = oneOf(net.lockReqs)
    compAckLockReq(r),
    nondet r = oneOf(net.lockAcks)
    bridgeRecvAck(r),
    nondet t = oneOf(net.holdQs)
    bridgeReportAwake(t),
    nondet t = oneOf(net.sleepOvers)
    compHoldRelease(t),
    nondet id = oneOf(net.asks)
    nondet allow = oneOf(Set(true, false))
    consentAnswer(id, allow),
    nondet a = oneOf(net.answers)
    compRecvAnswer(a),
    nondet t = oneOf(net.stops)
    compRecvStop(t),
  }

  /// Crashes, failures and events from outside.
  action stepFault = any {
    compRender(false),
    kmsFlipFail,
    outputOff,
    outputBack,
    compCloseAttempt,
    uiCrash,
    authdRestart,
    all { comp.inst < MAX_INST, compRestart },
    nondet drain = oneOf(Set(true, false))
    inhibitTimeout(drain),
    bridgeCrash,
    bridgeStart,
    bridgeFdLost,
    logindSleepFailed,
    compBlankOutput,
    compAskClose,
    all { world.win < MAX_WIN, windowReplaced },
    nondet g = oneOf(gh.issued)
    deadlinePasses(g),
    nondet h = oneOf(HOLDERS)
    all { world.conn.get(h) < MAX_CONN, holderRestart(h) },
    nondet h = oneOf(HOLDERS)
    userPressStop(h),
  }

  /// Repeated groups weight sampling toward the plain path. Their actual
  /// frequencies depend on the enabled branches and nondeterministic picks.
  /// Repetition changes no reachable state.
  action step = any {
    stepLock, stepLock, stepLock, stepLock,
    stepGrant, stepGrant, stepGrant,
    stepSleep, stepSleep,
    stepStart,
    stepStale,
    stepFault,
  }

  /// The fault storm gives each group one entry in the same transition relation.
  action stepStorm = any { stepLock, stepGrant, stepSleep, stepStart, stepStale, stepFault }

  // -------------------------------------------------------------- invariants

  /// A (REQ-AUTH-001): every unlock was caused by a positive authd
  /// verdict for the instance, epoch and open attempt current at that moment.
  val propA_verdict = not(gh.badUnlock)

  /// A, upstream half (REQ-AUTH-003): authd issues a positive verdict
  /// only after PAM succeeded for the same binding.
  val propA_pam = gh.okIssued.subseteq(gh.pamOk)

  /// A, end to end (REQ-AUTH-018): the user typed the right secret
  /// into the UI of the very attempt that unlocked.
  val propA_typed = not(gh.untypedUnlock)

  /// A across suspend (REQ-AUTH-016): no unlock on an
  /// attempt opened before the last resume.
  val propA_wake = not(gh.staleWakeUnlock)

  /// A across an authd disconnect (REQ-AUTH-017): no unlock on
  /// a verdict issued by an authd that has disconnected since.
  val propA_broker = not(gh.brokerUnlock)

  /// B (REQ-LOCK-002): from the acknowledgement until an unlock
  /// by verdict the panel never shows session content.
  val propB_covered = gh.mustCover implies kms.screen != SessionFrame

  /// B, compositor view: `Locked` means acknowledged and covered.
  val propB_phase =
    comp.phase == Locked implies (gh.mustCover and not(kms.busy and kms.pending == SessionFrame))

  /// B across suspend (REQ-LOCK-015, REQ-LOCK-016): after a
  /// resume no session frame is submitted and none is presented before an
  /// unlock by verdict in the same wake period. REQ-LOCK-015 also constrains
  /// the first submission when an authorized unlock happens first (see README).
  val propB_resume = not(gh.sessionAfterResume)

  /// REQ-LOCK-018: the delay inhibitor is released by an
  /// acknowledgement only if the compositor was `Locked` for that request.
  val propS_sleep = not(gh.badRelease)

  /// C (REQ-GRANT-013): nothing is delivered on a revoked grant.
  /// Lock, resume, restart, Stop, expiry, a replaced window, a reconnect and
  /// the use of a one-shot grant all count as revocation.
  val propC_noLateDelivery = gh.late == Set()

  /// C (REQ-LOCK-021): outside `Unlocked` the compositor holds no
  /// live grant and no open prompt.
  val propC_lockRevokes =
    comp.phase != Unlocked implies and {
      HOLDERS.forall(h => not(comp.grants.get(h).live)),
      not(comp.ask.open),
    }

  /// C (REQ-GRANT-015): every grant has the user's allow for
  /// exactly that prompt.
  val propC_consent = not(gh.unconsented)

  /// C (REQ-GRANT-008): a Stop ends only the stream generation it names.
  val propC_stopExact = not(gh.wrongStop)

  /// D (REQ-AGENT-001, REQ-AGENT-005): an agent receives capture
  /// or a transfer only on a live grant, and once per grant.
  val propD_agent = not(gh.reuse) and gh.late.forall(h => not(isAgent(h)))

  /// D in v1 (REQ-AGENT-001): without the option no prompt, grant
  /// or delivery for agent capture of a host window ever exists.
  val propD_v1 = AGENT_CAPTURE or and {
    comp.grants.get(AgentCapture) == DEAD,
    not(comp.ask.open and comp.ask.h == AgentCapture),
    world.queue.forall(j => j.h != AgentCapture),
  }

  /// A (REQ-AUTH-019): one authd instance sends each binding to pwcheck at
  /// most once, so one attempt is one password guess and one verdict.
  val propA_onePam = not(gh.doubleCheck)

  /// B (REQ-LOCK-015, REQ-LOCK-010): the first frame submitted after a
  /// resume, and after an output that went dark outside `Unlocked` returns,
  /// is a lock frame. No authorized unlock can come first.
  val propB_firstSubmit = not(gh.sessionFirst)

  /// B (REQ-LOCK-015, REQ-LOCK-010): `Locked` on an enabled output means the
  /// lock frame is on the panel. Attempts open and verdicts are accepted only
  /// in that state, so every unlock starts from a visible lock frame.
  val propB_shown = (comp.phase == Locked and kms.outputOn) implies kms.screen == LockFrame

  /// sleep (REQ-LOCK-026): from the acknowledgement of a request of the
  /// pending sleep cycle until that cycle ends, the session stays locked.
  val propS_committed = gh.committed implies comp.phase != Unlocked

  /// sleep (REQ-LOCK-018): at the moment a valid acknowledgement releases the
  /// inhibitor, the session is locked and the panel shows none of it.
  val propS_release = not(gh.uncoveredRelease)

  /// sleep (REQ-LOCK-018, REQ-LOCK-026): a sleep released by a valid
  /// acknowledgement, or acknowledged and then forced by logind's timer,
  /// begins with the panel covered.
  val propS_ackedSleepCovered =
    (sl.suspended and (gh.ackSleep or gh.committed)) implies kms.screen != SessionFrame

  /// sleep (REQ-LOCK-027): a bridge crash alone never releases the delay of a
  /// pending sleep.
  val propS_crashKeepsDelay = not(gh.crashRelease)

  /// trusted mode (REQ-TRUST-006): nothing is delivered to a grant holder
  /// while a consent prompt is open or the session is not unlocked.
  val propT_noDelivery = not(gh.promptDelivery)

  /// The fifteen predicates of the first validated revision. The gap traces
  /// (`cx_no_sleep_hold_release` and others) satisfy all of them, which is why
  /// the eight predicates added since exist.
  val coreSafety = and {
    propA_verdict, propA_pam, propA_typed, propA_wake, propA_broker,
    propB_covered, propB_phase, propB_resume, propS_sleep,
    propC_noLateDelivery, propC_lockRevokes, propC_consent, propC_stopExact,
    propD_agent, propD_v1,
  }

  val safety = and {
    propA_verdict, propA_pam, propA_typed, propA_wake, propA_broker, propA_onePam,
    propB_covered, propB_phase, propB_resume, propB_firstSubmit, propB_shown,
    propS_sleep, propS_committed, propS_release, propS_ackedSleepCovered,
    propS_crashKeepsDelay,
    propC_noLateDelivery, propC_lockRevokes, propC_consent, propC_stopExact,
    propD_agent, propD_v1, propT_noDelivery,
  }

  // ------------------------------------------- stronger, expected to fail
  // Not part of `safety`. No design can guarantee it while logind bounds the
  // delay: REQ-LOCK-012 states what holds instead (README.md, "Findings").

  /// The machine never sleeps with session content on the panel.
  val strongS_sleepCovered = sl.suspended implies kms.screen != SessionFrame

  /// The same, restricted to sleep cycles released by an acknowledgement.
  /// Kept under its old name: it is now implied by `propS_ackedSleepCovered`
  /// and holds on the chosen design.
  val strongS_ackedSleepCovered =
    (sl.suspended and gh.ackSleep) implies kms.screen != SessionFrame

  // --------------------------------------------------------------- witnesses
  // These predicates measure coverage. Agent capture is reachable only after v1.
  val witUnlock = gh.sawUnlock
  val witAck = gh.sawAck
  val witDelivery = gh.sawDelivery
  val witSleepByAck = gh.sawSleepByAck
  val witNegativeVerdict = gh.sawNegativeVerdict
  val witDrop = gh.sawDrop
  val witGrant = gh.issued != Set()
  val witSecondEpoch = comp.epoch >= 2
  val witSecondInstance = comp.inst >= 2
  val witUnlockAfterResume = gh.sawUnlockAfterResume
  val witStaleFlip = kms.busy and kms.resolved
  val witStop = net.stops != Set()
  val witTransferGrant = comp.grants.get(AgentTransfer).live
  val witCaptureGrant = comp.grants.get(AgentCapture).live
  val witHold = comp.hold
  val witCommitted = gh.committed
  val witHoldReport = gh.sawHoldReport
  val witSleepFailed = gh.sawSleepFailed
  val witSuppressed = gh.sawSuppressed
  val witAckedSuspend = sl.suspended and gh.ackSleep
  val witUncoveredSuspend = sl.suspended and kms.screen == SessionFrame
  val witStaleRecord = HOLDERS.exists(h =>
    comp.grants.get(h).live and comp.grants.get(h).conn != world.conn.get(h))

  // Whether a run reached each bound of `step`.
  val reachInst = comp.inst == MAX_INST
  val reachEpoch = comp.epoch >= MAX_EPOCH
  val reachAttempt = comp.attempt == MAX_ATTEMPT
  val reachAsk = comp.askN == MAX_ASK
  val reachGrant = world.nextGen == MAX_GRANT
  val reachJob = world.nextJob == MAX_JOB
  val reachWin = world.win == MAX_WIN
  val reachConn = HOLDERS.exists(h => world.conn.get(h) == MAX_CONN)
  val reachSleep = sl.cycles == MAX_SLEEP
  val reachReq = sl.nextReq == MAX_REQ

  // ---------------------------------------------------------------- progress
  // Liveness as a bounded progress check: from any state the random prefix
  // reaches, a fault-free schedule acknowledges a lock, and unlocks for a user
  // who types the right secret, within a fixed number of steps.

  action stutter = all {
    comp' = comp, kms' = kms, sl' = sl, auth' = auth, world' = world, net' = net, gh' = gh,
  }

  action unlockProgress = {
    val b = bindOf(comp)
    if (not(comp.open)) compOpenAttempt
    else if (net.verdicts.contains({ b: b, ok: true })) compRecvVerdict({ b: b, ok: true })
    else if (auth.ch != b) authdRecvChallenge(b)
    else if (auth.used and net.pams.contains({ b: b, ok: true })) authdRecvPam({ b: b, ok: true })
    else if (auth.used and net.checks.contains({ b: b, good: true })) pwcheckRun({ b: b, good: true })
    else if (auth.used) compCloseAttempt
    else if (net.creds.contains({ b: b, good: true })) authdRecvCred({ b: b, good: true })
    else if (auth.uiUp and auth.uiFor == b) userType(true)
    else compCloseAttempt
  }

  action sleepProgress =
    if (sl.bridgeUp and sl.req == 0) bridgeSeeSleep
    else if (sl.bridgeUp and sl.inhibit and net.lockAcks.contains(sl.req)) bridgeRecvAck(sl.req)
    else if (sl.bridgeUp and sl.inhibit) compAckLockReq(sl.req)
    else stutter

  /// REQ-LOCK-026: finish the sleep cycle or learn that there is none.
  /// systemd restarts the bridge; logind suspends once the inhibitor is gone;
  /// without a pending sleep the bridge takes its inhibitor and reports.
  action holdProgress =
    if (not(sl.bridgeUp)) bridgeStart
    else if (sl.pending and not(sl.inhibit)) logindSuspend(true)
    else if (sl.pending) sleepProgress
    else if (not(sl.inhibit)) bridgeRearm
    else if (net.sleepOvers.contains(tokOf(comp))) compHoldRelease(tokOf(comp))
    else bridgeReportAwake(tokOf(comp))

  action progressStep(wantUnlock: bool): bool =
    if (sl.suspended) resume
    else if (sl.jump) compSeeJump
    // REQ-LOCK-015: no unlock from a dark panel; input switches the output on.
    else if (wantUnlock and comp.phase != Unlocked and not(kms.outputOn)) outputBack
    else if (comp.phase == Locking and not(kms.outputOn)) compAckNoOutput
    else if (kms.busy and kms.resolved) compReadStaleFlip
    else if (comp.phase != Unlocked and kms.busy) kmsFlipDone
    else if (comp.phase == Locking) compRender(true)
    else if (comp.phase == Locked and wantUnlock and comp.hold) holdProgress
    else if (comp.phase == Locked and wantUnlock) unlockProgress
    else if (comp.phase == Locked and sl.pending) sleepProgress
    else stutter

  action lockIfUnlocked =
    if (sl.suspended) resume
    else if (sl.jump) compSeeJump
    else if (comp.phase == Unlocked) compLock
    else stutter

  /// lockAckProgress (proposed): a lock request is acknowledged, the panel is
  /// covered, and a pending sleep is released, within 12 fault-free steps.
  run lockAckProgressTest =
    init
      .then(40.reps(_ => step))
      .then(3.reps(_ => lockIfUnlocked))
      .then(9.reps(_ => progressStep(false)))
      .expect(and {
        comp.phase == Locked,
        gh.mustCover,
        kms.screen != SessionFrame,
        (sl.pending and sl.bridgeUp) implies not(sl.inhibit),
        safety,
      })

  /// unlockProgress (proposed): with the right secret the session unlocks
  /// within UNLOCK_STEPS fault-free steps from each sampled 40-step prefix.
  /// The schedule may have to finish a sleep cycle or wait for the bridge's
  /// report first (REQ-LOCK-026), and present a fresh lock frame (REQ-LOCK-015).
  pure val UNLOCK_STEPS = 24

  run unlockProgressTest =
    init
      .then(40.reps(_ => step))
      .then(UNLOCK_STEPS.reps(_ => progressStep(true)))
      .expect(comp.phase == Unlocked and safety)

  // ----------------------------------------------------------- scenario tests

  pure val B11: Bind = { inst: 1, epoch: 1, attempt: 1 }

  pure val B12: Bind = { inst: 1, epoch: 1, attempt: 2 }
  pure val OK11: Result = { b: B11, ok: true }
  pure val ASK1: AskId = { inst: 1, n: 1 }

  /// From `Unlocked` to an acknowledged lock.
  run toLocked = compLock.then(compRender(true)).then(kmsFlipDone)

  /// Issue a positive verdict for an already open attempt.
  run issuePositive(b: Bind): bool =
    authdRecvChallenge(b)
      .then(userType(true))
      .then(authdRecvCred({ b: b, good: true }))
      .then(pwcheckRun({ b: b, good: true }))
      .then(authdRecvPam({ b: b, ok: true }))

  /// From a fresh `Locked` to a positive verdict for (1, 1, 1) in flight.
  run toVerdict11 = compOpenAttempt.then(issuePositive(B11))

  /// First grant of the run (generation 1) for holder `h`.
  run toGrant(h: Holder): bool =
    compAsk(h).then(consentAnswer(ASK1, true)).then(compRecvAnswer({ id: ASK1, allow: true }))

  /// REQ-LOCK-002, REQ-AUTH-001: the plain path.
  run happyPathTest =
    init
      .then(compLock)
      .expect(comp.phase == Locking and comp.epoch == 1)
      .then(compRender(true))
      .expect(comp.phase == Locking and kms.screen == SessionFrame)
      .then(kmsFlipDone)
      .expect(comp.phase == Locked and kms.screen == LockFrame)
      .then(compOpenAttempt)
      .then(authdRecvChallenge(B11))
      .then(userType(true))
      .then(authdRecvCred({ b: B11, good: true }))
      .then(pwcheckRun({ b: B11, good: true }))
      .then(authdRecvPam({ b: B11, ok: true }))
      .then(compRecvVerdict({ b: B11, ok: true }))
      .expect(comp.phase == Unlocked and gh.sawUnlock and safety)

  /// REQ-LOCK-002: a session frame queued before the lock
  /// completes after it; that flip must not acknowledge the lock.
  run staleFlipDoesNotAckTest =
    init
      .then(compRender(true))
      .then(compLock)
      .then(kmsFlipDone)
      .expect(comp.phase == Locking and kms.screen == SessionFrame and not(gh.mustCover))
      .then(compRender(true))
      .then(kmsFlipFail)
      .expect(comp.phase == Locking)
      .then(compRender(true))
      .then(kmsFlipDone)
      .expect(comp.phase == Locked and kms.screen == LockFrame and safety)

  /// REQ-AUTH-001: a wrong secret consumes the attempt; the
  /// negative verdict cannot be followed by a positive one for the same attempt.
  run wrongSecretStaysLockedTest =
    init
      .then(toLocked)
      .then(compOpenAttempt)
      .then(authdRecvChallenge(B11))
      .then(userType(false))
      .then(authdRecvCred({ b: B11, good: false }))
      .then(pwcheckRun({ b: B11, good: false }))
      .then(authdRecvPam({ b: B11, ok: false }))
      .then(compRecvVerdict({ b: B11, ok: false }))
      .expect(comp.phase == Locked and not(comp.open) and safety)

  /// REQ-LOCK-015: sleep after a missed lock (inhibitor timeout);
  /// the first frame the compositor submits after resume is a lock frame.
  run resumeRelocksTest =
    init
      .then(logindPrepareSleep)
      .then(inhibitTimeout(true))
      .then(resume)
      .expect(comp.phase == Unlocked and not(clockOk))
      .then(compSeeJump)
      .expect(comp.phase == Locking)
      .then(compRender(true))
      .then(kmsFlipDone)
      .expect(comp.phase == Locked and kms.screen == LockFrame and safety)

  /// REQ-LOCK-018: the plain sleep handshake.
  run sleepHandshakeTest =
    init
      .then(logindPrepareSleep)
      .then(bridgeSeeSleep)
      .then(toLocked)
      .then(compAckLockReq(1))
      .then(bridgeRecvAck(1))
      .then(logindSuspend(true))
      .expect(sl.suspended and kms.screen == LockFrame and gh.sawSleepByAck and safety)

  /// REQ-GRANT-013, REQ-LOCK-021: an effect queued
  /// before the lock is not delivered after it.
  run queuedDeliveryDroppedByLockTest =
    init
      .then(toGrant(Portal))
      .then(compEnqueue(Portal))
      .then(compDispatch({ h: Portal, gen: 1, seq: 1 }))
      .expect(gh.sawDelivery and comp.grants.get(Portal).live)
      .then(compEnqueue(Portal))
      .then(compLock)
      .expect(not(comp.grants.get(Portal).live) and world.queue.size() == 1)
      .then(compDispatch({ h: Portal, gen: 1, seq: 2 }))
      .expect(world.queue == Set() and gh.sawDrop and gh.used == Set(1) and safety)

  /// REQ-AGENT-005: two queued deliveries on one agent grant, one arrives.
  run agentOneShotTest =
    init
      .then(toGrant(AgentTransfer))
      .then(compEnqueue(AgentTransfer))
      .then(compEnqueue(AgentTransfer))
      .then(compDispatch({ h: AgentTransfer, gen: 1, seq: 1 }))
      .expect(gh.sawDelivery and not(gh.sawDrop) and not(comp.grants.get(AgentTransfer).live))
      .then(compDispatch({ h: AgentTransfer, gen: 1, seq: 2 }))
      .expect(gh.sawDrop and safety)

  // ---------------------------------------------- counterexamples and guards
  // `cx_<defect>` is a short trace that breaks a property once the rule
  // named by the defect is removed; it passes only in that variant
  // (variants.qnt runs it as `counterexampleTest`). The `...GuardTest` next to
  // it replays the same trace on the chosen design and shows the rule holding.
  // Each pair is one future Rust reducer test.

  run cx_client_unlock =
    init.then(toLocked).then(uiUnlockAndDestroy)
      .expect(comp.phase == Unlocked and not(propA_verdict))

  /// REQ-LOCK-008
  run uiCannotUnlockGuardTest =
    init.then(toLocked).then(uiUnlockAndDestroy.fail())

  run cx_honor_logind_unlock =
    init.then(toLocked).then(logindUnlockSignal)
      .expect(comp.phase == Unlocked and not(propA_verdict))

  /// REQ-LOCK-017
  run logindUnlockIgnoredGuardTest =
    init.then(toLocked).then(logindUnlockSignal.fail())

  run cx_unlock_on_render_error =
    init.then(compLock).then(compRender(false))
      .expect(comp.phase == Unlocked and not(propA_verdict))

  /// REQ-LOCK-011
  run renderErrorStaysLockingGuardTest =
    init.then(compLock).then(compRender(false))
      .expect(comp.phase == Locking and safety)

  pure val T11: Tok = { inst: 1, n: 1 }
  pure val T20: Tok = { inst: 2, n: 0 }
  pure val T21: Tok = { inst: 2, n: 1 }

  /// REQ-LOCK-026: a new instance leaves its start hold on the bridge's
  /// report for its first token. One round trip, no sleep pending.
  run leaveStartHold = bridgeReportAwake(T20).then(compHoldRelease(T20))

  run instancePrefix =
    init.then(toLocked).then(toVerdict11).then(compRecvVerdict(OK11))
      .then(compRestart).then(compRender(true)).then(kmsFlipDone)
      .expect(comp.hold and tokOf(comp) == T20)
      .then(leaveStartHold).then(compOpenAttempt)
      .expect(bindOf(comp) == { inst: 2, epoch: 1, attempt: 1 } and comp.phase == Locked)

  run cx_skip_instance_check =
    instancePrefix.then(compRecvVerdict(OK11))
      .expect(comp.phase == Unlocked and not(propA_verdict))

  /// REQ-AUTH-001: even when both instances have issued a positive verdict,
  /// the evidence for an unlock must identify the message that caused it.
  run cx_coincident_verdict = {
    val current = { ...B11, inst: 2 }
    instancePrefix.then(issuePositive(current))
      .expect(gh.okIssued.contains(B11) and gh.okIssued.contains(current))
      .then(compRecvVerdict(OK11))
      .expect(not(propA_verdict))
  }

  /// REQ-GRANT-015: an old Allow cannot authorize a new prompt even if
  /// the user separately allowed both. Both answers are in flight at delivery.
  run cx_coincident_allow = {
    val current = { inst: 2, n: 1 }
    init.then(compAsk(Portal)).then(consentAnswer(ASK1, true))
      .then(compRestart).then(compRender(true)).then(kmsFlipDone)
      .then(leaveStartHold)
      .then(compOpenAttempt).then(issuePositive({ ...B11, inst: 2 }))
      .then(compRecvVerdict({ b: { ...B11, inst: 2 }, ok: true }))
      .then(compAsk(Portal)).then(consentAnswer(current, true))
      .expect(gh.allowed == Set(ASK1, current))
      .then(compRecvAnswer({ id: ASK1, allow: true }))
      .expect(comp.grants.get(Portal).live and not(propC_consent))
  }

  /// REQ-AUTH-001, REQ-AUTH-015: counters restart with the compositor, so only
  /// the instance id tells an old verdict from a new one.
  run staleInstanceVerdictGuardTest =
    instancePrefix.then(compRecvVerdict(OK11).fail())

  run epochPrefix =
    init.then(toLocked).then(toVerdict11).then(compRecvVerdict(OK11))
      .then(toLocked).then(compOpenAttempt)
      .expect(bindOf(comp) == { inst: 1, epoch: 2, attempt: 1 } and comp.phase == Locked)

  run cx_skip_epoch_check =
    epochPrefix.then(compRecvVerdict(OK11))
      .expect(comp.phase == Unlocked and not(propA_verdict))

  /// REQ-AUTH-001: a duplicate of the
  /// verdict that ended epoch 1 does not end epoch 2.
  run staleEpochVerdictGuardTest =
    epochPrefix.then(compRecvVerdict(OK11).fail())

  run attemptPrefix =
    init.then(toLocked).then(toVerdict11).then(compCloseAttempt)

  run cx_skip_attempt_check =
    attemptPrefix.then(compRecvVerdict(OK11))
      .expect(comp.phase == Unlocked and not(propA_verdict))

  /// REQ-AUTH-004: a verdict that arrives after its attempt
  /// timed out does not unlock.
  run lateVerdictAfterTimeoutGuardTest =
    attemptPrefix.then(compRecvVerdict(OK11).fail())

  run channelPrefix =
    init.then(toLocked)
      .then(compOpenAttempt).then(userType(true)).then(compCloseAttempt)
      .then(compOpenAttempt).then(authdRecvChallenge(B12))

  run cx_shared_ui_channel =
    channelPrefix
      .then(authdRecvCred({ b: B11, good: true }))
      .then(pwcheckRun({ b: B12, good: true }))
      .then(authdRecvPam({ b: B12, ok: true }))
      .then(compRecvVerdict({ b: B12, ok: true }))
      .expect(comp.phase == Unlocked and propA_verdict and not(propA_typed))

  /// REQ-AUTH-018: a secret typed into the UI of attempt 1 is not
  /// judged for attempt 2.
  run staleCredentialGuardTest =
    channelPrefix.then(authdRecvCred({ b: B11, good: true }).fail())

  /// Guard: `happyPathTest` (the lock is still `Locking` after the submit).
  run cx_ack_on_submit =
    init.then(compLock).then(compRender(true))
      .expect(comp.phase == Locked and kms.screen == SessionFrame and not(propB_covered))

  /// Guard: `staleFlipDoesNotAckTest`.
  run cx_ack_any_flip =
    init.then(compRender(true)).then(compLock).then(kmsFlipDone)
      .expect(comp.phase == Locked and kms.screen == SessionFrame and not(propB_covered))

  run boottimePrefix =
    init.then(logindPrepareSleep).then(inhibitTimeout(true)).then(resume)

  run cx_no_boottime =
    boottimePrefix.then(compRender(true)).then(kmsFlipDone)
      .expect(comp.phase == Unlocked and not(propB_resume))

  /// REQ-LOCK-025: after a resume nothing is rendered before the
  /// clock jump is handled.
  run noFrameBeforeJumpGuardTest =
    boottimePrefix.then(compRender(true).fail())

  /// The lock frame is presented again after the resume, so that only the
  /// closed attempt stands between the old verdict and an unlock.
  run resumeAttemptPrefix =
    init.then(toLocked).then(toVerdict11)
      .then(logindPrepareSleep).then(inhibitTimeout(true)).then(resume).then(compSeeJump)
      .then(compRender(true)).then(kmsFlipDone)
      .expect(comp.phase == Locked and kms.screen == LockFrame)

  run cx_keep_attempt_on_resume =
    resumeAttemptPrefix.then(compRecvVerdict(OK11))
      .expect(comp.phase == Unlocked and not(propA_wake))

  /// REQ-AUTH-016: a secret typed before the lid closed
  /// does not unlock for whoever opens it.
  run verdictAcrossSleepGuardTest =
    resumeAttemptPrefix.then(compRecvVerdict(OK11).fail())

  run staleAckPrefix =
    init
      .then(logindPrepareSleep).then(bridgeSeeSleep).then(toLocked)
      .then(compAckLockReq(1)).then(bridgeRecvAck(1)).then(logindSuspend(true))
      .then(resume).then(compSeeJump).then(compRender(true)).then(kmsFlipDone)
      .then(toVerdict11).then(compRecvVerdict(OK11))
      .then(bridgeRearm).then(logindPrepareSleep).then(bridgeSeeSleep)
      .expect(comp.phase == Unlocked and sl.req == 2 and sl.inhibit)

  run cx_stale_ack =
    staleAckPrefix.then(bridgeRecvAck(1)).then(logindSuspend(true))
      .expect(sl.suspended and comp.phase == Unlocked and not(propS_sleep))

  /// REQ-LOCK-024: the second request already has a matching acknowledgement,
  /// but it is the nonce of the message actually received that authorizes release.
  run cx_coincident_ack =
    staleAckPrefix.then(toLocked).then(compAckLockReq(2))
      .expect(net.lockAcks == Set(1, 2) and gh.lockedSinceReq)
      .then(bridgeRecvAck(1))
      .expect(not(sl.inhibit) and not(propS_sleep))

  /// REQ-LOCK-024: the acknowledgement of an earlier sleep
  /// cycle does not release the inhibitor of this one.
  run staleLockAckGuardTest =
    staleAckPrefix.then(bridgeRecvAck(1).fail())

  /// Guard: `queuedDeliveryDroppedByLockTest`.
  run cx_no_revoke_on_lock =
    init.then(toGrant(Portal)).then(compLock)
      .expect(comp.grants.get(Portal).live and not(propC_lockRevokes))

  /// Guard: `queuedDeliveryDroppedByLockTest`.
  run cx_check_at_enqueue_only =
    init.then(toGrant(Portal)).then(compEnqueue(Portal)).then(compLock)
      .then(compDispatch({ h: Portal, gen: 1, seq: 1 }))
      .expect(comp.phase == Locking and gh.sawDelivery and not(propC_noLateDelivery))

  /// Guard: `agentOneShotTest`.
  run cx_check_at_enqueue_only_agent =
    init.then(toGrant(AgentTransfer))
      .then(compEnqueue(AgentTransfer)).then(compEnqueue(AgentTransfer))
      .then(compDispatch({ h: AgentTransfer, gen: 1, seq: 1 }))
      .then(compDispatch({ h: AgentTransfer, gen: 1, seq: 2 }))
      .expect(not(propD_agent))

  run reconnectPrefix =
    init.then(toGrant(Portal)).then(compEnqueue(Portal)).then(holderRestart(Portal))
      .then(compDispatch({ h: Portal, gen: 1, seq: 1 }))

  run cx_grant_by_name = reconnectPrefix.expect(not(propC_noLateDelivery))

  /// REQ-GRANT-014: a grant belongs to one connection, not to a name.
  run reconnectDropsDeliveryGuardTest =
    reconnectPrefix.expect(gh.sawDrop and not(gh.sawDelivery) and safety)

  run expiryPrefix =
    init.then(toGrant(Portal)).then(compEnqueue(Portal)).then(deadlinePasses(1))
      .then(compDispatch({ h: Portal, gen: 1, seq: 1 }))

  run cx_timer_only_expiry = expiryPrefix.expect(not(propC_noLateDelivery))

  /// REQ-GRANT-013: the deadline is read from the clock when
  /// the effect runs, before the expiry timer has fired.
  run expiredDeliveryDroppedGuardTest =
    expiryPrefix.expect(gh.sawDrop and not(gh.sawDelivery) and safety)

  run windowPrefix =
    init.then(toGrant(Portal)).then(compEnqueue(Portal)).then(windowReplaced)
      .then(compDispatch({ h: Portal, gen: 1, seq: 1 }))

  run cx_skip_window_gen = windowPrefix.expect(not(propC_noLateDelivery))

  /// REQ-GRANT-007
  run replacedWindowDropsDeliveryGuardTest =
    windowPrefix.expect(gh.sawDrop and not(gh.sawDelivery) and safety)

  /// REQ-GRANT-007: consent given for a window that was replaced
  /// while the prompt was open creates no grant.
  run consentForReplacedWindowGuardTest =
    init.then(compAsk(AgentTransfer)).then(windowReplaced).then(consentAnswer(ASK1, true))
      .then(compRecvAnswer({ id: ASK1, allow: true }).fail())

  run brokerPrefix = init.then(toLocked).then(toVerdict11).then(authdRestart)

  run cx_keep_attempt_on_authd_disconnect =
    brokerPrefix.then(compRecvVerdict(OK11))
      .expect(comp.phase == Unlocked and propA_verdict and not(propA_broker))

  /// REQ-AUTH-017: a success issued before authd disconnected
  /// does not unlock afterwards. Same epoch, same attempt number, no re-lock.
  run verdictAfterBrokerDisconnectGuardTest =
    brokerPrefix
      .expect(comp.phase == Locked and comp.epoch == 1 and comp.attempt == 1 and not(comp.open))
      .then(compRecvVerdict(OK11).fail())

  /// The lock frame is pending when the machine goes to sleep and the kernel
  /// drops it; the session frame stays on the panel.
  run staleFlipPrefix =
    init.then(compLock).then(compRender(true))
      .then(logindPrepareSleep).then(inhibitTimeout(false)).then(resume).then(compSeeJump)
      .expect(kms.busy and kms.resolved and kms.screen == SessionFrame)
      .then(compReadStaleFlip)

  run cx_ack_stale_flip =
    staleFlipPrefix.expect(comp.phase == Locked and not(propB_covered))

  /// REQ-LOCK-015: the event of a flip from before the sleep does not
  /// acknowledge the lock; the next flip does.
  run staleFlipAfterResumeGuardTest =
    staleFlipPrefix
      .expect(comp.phase == Locking and not(gh.mustCover) and safety)
      .then(compRender(true)).then(kmsFlipDone)
      .expect(comp.phase == Locked and kms.screen == LockFrame and safety)

  /// REQ-LOCK-015: only the completion event survives a settled flip.
  /// A second suspend before that event is read cannot present a discarded frame.
  run settledFlipAcrossTwoSleepsTest =
    init.then(compLock).then(compRender(true))
      .then(logindPrepareSleep).then(inhibitTimeout(false)).then(resume)
      .expect(kms.resolved and kms.pending == LockFrame and kms.screen == SessionFrame)
      .then(logindPrepareSleep).then(inhibitTimeout(true))
      .expect(kms.resolved and kms.screen == SessionFrame and safety)

  /// REQ-LOCK-015: a session frame pending at
  /// the suspend is shown before the sleep or never. The first frame that
  /// reaches the panel after the resume is a lock frame.
  run firstPresentationAfterResumeTest =
    init.then(compRender(true))
      .then(logindPrepareSleep).then(inhibitTimeout(true)).then(resume)
      .expect(kms.busy and kms.resolved)
      .then(kmsFlipDone.fail())

  run firstPresentationAfterResume2Test =
    init.then(compRender(true))
      .then(logindPrepareSleep).then(inhibitTimeout(true)).then(resume)
      .then(compSeeJump).then(compReadStaleFlip)
      .expect(comp.phase == Locking and not(kms.busy))
      .then(compRender(true)).then(kmsFlipDone)
      .expect(comp.phase == Locked and kms.screen == LockFrame and safety)

  pure val ASK2: AskId = { inst: 1, n: 2 }

  /// A Stop for generation 1 is still in flight when the holder is granted
  /// generation 2.
  run stopPrefix =
    init.then(toGrant(Portal)).then(userPressStop(Portal))
      .then(compAsk(Portal)).then(consentAnswer(ASK2, true))
      .then(compRecvAnswer({ id: ASK2, allow: true }))
      .expect(comp.grants.get(Portal).gen == 2 and comp.grants.get(Portal).live)

  run cx_stop_by_holder =
    stopPrefix.then(compRecvStop({ h: Portal, gen: 1 }))
      .expect(not(comp.grants.get(Portal).live) and not(propC_stopExact))

  /// REQ-GRANT-008: a Stop is bound to the stream generation.
  run staleStopGuardTest =
    stopPrefix.then(compRecvStop({ h: Portal, gen: 1 }).fail())

  /// REQ-GRANT-008: Stop ends its stream; a queued delivery is then dropped.
  run stopEndsStreamTest =
    init.then(toGrant(Portal)).then(compEnqueue(Portal)).then(userPressStop(Portal))
      .then(compRecvStop({ h: Portal, gen: 1 }))
      .then(compDispatch({ h: Portal, gen: 1, seq: 1 }))
      .expect(gh.sawDrop and not(gh.sawDelivery) and safety)

  /// REQ-AGENT-001: in v1 a request for agent capture of a host
  /// window is refused. With the post-v1 option it is one-shot like a transfer.
  run agentCaptureGateTest =
    if (AGENT_CAPTURE)
      init.then(toGrant(AgentCapture))
        .then(compEnqueue(AgentCapture)).then(compEnqueue(AgentCapture))
        .then(compDispatch({ h: AgentCapture, gen: 1, seq: 1 }))
        .then(compDispatch({ h: AgentCapture, gen: 1, seq: 2 }))
        .expect(gh.used == Set(1) and gh.sawDrop and safety)
    else
      init.then(compAsk(AgentCapture).fail())

  // --------------------------------------------- sleep hold (REQ-LOCK-026)

  /// A positive verdict for (1,1,1) is in flight when the compositor
  /// acknowledges the lock request of the pending sleep.
  run ackPrefix =
    init
      .then(logindPrepareSleep).then(bridgeSeeSleep).then(toLocked).then(toVerdict11)
      .then(compAckLockReq(1))

  /// REQ-LOCK-018 without the hold. The verdict is valid and arrives after
  /// the acknowledgement was sent. The session unlocks and is presented, and
  /// the bridge then releases the inhibitor on the correct acknowledgement
  /// with the session on the panel (16 steps). The first fifteen predicates
  /// all hold.
  run cx_no_sleep_hold_release =
    ackPrefix.then(compRecvVerdict(OK11))
      .then(compRender(true)).then(kmsFlipDone).then(bridgeRecvAck(1))
      .expect(not(sl.inhibit) and kms.screen == SessionFrame and coreSafety)
      .expect(not(propS_committed) and not(propS_release))

  /// The same one step later: release first, then the verdict. The machine
  /// suspends with the session on the panel (17 steps), and the session frame
  /// is still there after the resume until the lock frame is presented.
  run cx_no_sleep_hold_suspend =
    ackPrefix.then(bridgeRecvAck(1))
      .then(compRecvVerdict(OK11)).then(compRender(true)).then(kmsFlipDone)
      .then(logindSuspend(true))
      .expect(coreSafety and propS_release)
      .expect(not(propS_committed) and not(propS_ackedSleepCovered))
      .expect(not(strongS_ackedSleepCovered))
      .then(resume).then(compSeeJump)
      .expect(comp.phase == Locking and kms.screen == SessionFrame)

  /// REQ-LOCK-026: the acknowledgement closed the attempt and set the hold.
  /// The verdict is refused before the bridge has read the acknowledgement.
  run ackCommitsGuardTest =
    ackPrefix
      .expect(comp.hold and not(comp.open) and gh.committed and safety)
      .then(compRecvVerdict(OK11).fail())

  /// REQ-LOCK-026: and no new attempt opens.
  run noAttemptInHoldGuardTest =
    ackPrefix.then(compOpenAttempt.fail())

  /// REQ-LOCK-026: the verdict is refused after the release as well.
  run sleepHoldClosesGapGuardTest =
    ackPrefix.then(bridgeRecvAck(1))
      .expect(comp.hold and not(comp.open) and not(sl.inhibit))
      .then(compRecvVerdict(OK11).fail())

  /// REQ-LOCK-018, REQ-LOCK-026: the acknowledged sleep begins covered.
  run ackedSleepCoveredTest =
    ackPrefix.then(bridgeRecvAck(1)).then(logindSuspend(true))
      .expect(sl.suspended and gh.ackSleep and kms.screen == LockFrame and safety)

  /// The compositor restarts between its acknowledgement and the suspend.
  run restartPrefix =
    init
      .then(logindPrepareSleep).then(bridgeSeeSleep).then(toLocked)
      .then(compAckLockReq(1)).then(compRestart)
      .then(compRender(true)).then(kmsFlipDone).then(bridgeRecvAck(1))
      .expect(comp.phase == Locked and not(sl.inhibit) and gh.committed)

  /// A hold kept in memory only: the new instance knows of no sleep, and a
  /// valid unlock follows. The simulator found this trace first.
  run cx_hold_memory_only = {
    val b = { inst: 2, epoch: 1, attempt: 1 }
    restartPrefix.then(compOpenAttempt).then(issuePositive(b))
      .then(compRecvVerdict({ b: b, ok: true }))
      .expect(comp.phase == Unlocked and coreSafety and not(propS_committed))
      .then(compRender(true)).then(kmsFlipDone).then(logindSuspend(true))
      .expect(not(propS_ackedSleepCovered))
  }

  /// REQ-LOCK-026: a new instance starts in the hold and opens no attempt.
  run sleepHoldSurvivesRestartGuardTest =
    restartPrefix.expect(comp.hold and safety).then(compOpenAttempt.fail())

  /// The bridge answers the first token of a new instance while no sleep is
  /// pending. The report is still in flight when the next sleep is acknowledged.
  run staleReportPrefix =
    init.then(compRestart).then(bridgeReportAwake(T20))
      .then(compRender(true)).then(kmsFlipDone)
      .then(logindPrepareSleep).then(bridgeSeeSleep).then(compAckLockReq(1))
      .expect(comp.hold and tokOf(comp) == T21 and gh.committed)

  /// A report accepted whatever token it echoes ends the hold of a sleep that
  /// is still pending (15 steps).
  run cx_hold_any_report = {
    val b = { inst: 2, epoch: 1, attempt: 1 }
    staleReportPrefix.then(compHoldRelease(T20))
      .then(compOpenAttempt).then(issuePositive(b))
      .then(compRecvVerdict({ b: b, ok: true }))
      .expect(comp.phase == Unlocked and coreSafety and not(propS_committed))
  }

  /// REQ-LOCK-026: a report for an earlier token does not end the hold.
  run staleSleepOverGuardTest =
    staleReportPrefix.then(compHoldRelease(T20).fail())

  /// REQ-LOCK-026: the clock jump of the resume ends the hold. Unlock then
  /// needs the lock frame of this wake period and nothing from the bridge.
  run holdEndsOnResumeTest =
    ackPrefix.then(bridgeRecvAck(1)).then(logindSuspend(true))
      .then(resume).then(compSeeJump)
      .expect(not(comp.hold) and comp.phase == Locking and not(sl.inhibit))
      .then(compRender(true)).then(kmsFlipDone)
      .then(compOpenAttempt).then(issuePositive(B12))
      .then(compRecvVerdict({ b: B12, ok: true }))
      .expect(comp.phase == Unlocked and gh.sawUnlockAfterResume and safety)

  /// The sleep job fails after the release: no suspend, no clock jump.
  run failedSleepPrefix =
    ackPrefix.then(bridgeRecvAck(1)).then(logindSleepFailed)
      .expect(comp.hold and not(sl.inhibit) and not(sl.pending))

  /// REQ-LOCK-026: the bridge reports only once it holds an inhibitor again.
  run reportNeedsInhibitorGuardTest =
    failedSleepPrefix.then(bridgeReportAwake(T11).fail())

  /// REQ-LOCK-026: after a failed sleep the hold ends on the bridge's report,
  /// two bridge steps and one message after logind's PrepareForSleep(false).
  run holdEndsAfterFailedSleepTest =
    failedSleepPrefix
      .then(bridgeRearm).then(bridgeReportAwake(T11)).then(compHoldRelease(T11))
      .expect(not(comp.hold) and comp.phase == Locked and gh.sawHoldReport)
      .then(compOpenAttempt).then(issuePositive(B12))
      .then(compRecvVerdict({ b: B12, ok: true }))
      .expect(comp.phase == Unlocked and safety)

  /// A bad place to want to unlock from: the sleep is acknowledged and still
  /// pending, the bridge has crashed, the output is off, and the attempt the
  /// user was typing into was closed by the acknowledgement.
  run worstCasePrefix =
    ackPrefix.then(bridgeCrash).then(outputOff)
      .expect(comp.hold and sl.pending and sl.inhibit and not(sl.bridgeUp))

  /// unlockProgress, the price of REQ-LOCK-026 and REQ-LOCK-015 in steps:
  /// 19 fault-free steps. Output on and lock frame (3), bridge restart and a
  /// new acknowledged request (4), suspend, resume and jump (3), the lock
  /// frame of the new wake period (2), a new attempt (7).
  run worstCaseUnlockTest =
    worstCasePrefix.then(19.reps(_ => progressStep(true)))
      .expect(comp.phase == Unlocked and gh.sawUnlockAfterResume and safety)

  /// The 19 steps are needed: after 18 the verdict is still in flight.
  run worstCaseUnlockLowerBoundTest =
    worstCasePrefix.then(18.reps(_ => progressStep(true)))
      .expect(comp.phase == Locked and comp.open and safety)

  // ------------------------------- sleep without an acknowledgement (REQ-LOCK-012)

  /// The residual risk, stated as a test. logind's timer runs out before any
  /// acknowledgement and the machine sleeps with the session on the panel
  /// (2 steps). Every predicate of `safety` holds; `strongS_sleepCovered`
  /// does not, and no rule of the compositor can make it. On wake the
  /// compositor locks before anything else and submits a lock frame first.
  run unackedSleepResidualTest =
    init.then(logindPrepareSleep).then(inhibitTimeout(true))
      .expect(sl.suspended and kms.screen == SessionFrame and not(gh.committed))
      .expect(safety and not(strongS_sleepCovered))
      .then(resume).then(compSeeJump)
      .expect(comp.phase == Locking and safety)
      .then(compRender(true))
      .expect(kms.pending == LockFrame and safety)

  run bridgeCrashPrefix = init.then(logindPrepareSleep).then(bridgeCrash)

  /// Without the descriptor store a bridge crash releases the delay at once
  /// and the machine sleeps with the session on the panel (3 steps).
  run cx_no_fd_store =
    bridgeCrashPrefix.then(logindSuspend(true))
      .expect(sl.suspended and kms.screen == SessionFrame and coreSafety)
      .expect(not(propS_crashKeepsDelay))

  /// REQ-LOCK-027: the delay outlives the bridge process.
  run bridgeCrashKeepsDelayGuardTest =
    bridgeCrashPrefix.expect(sl.inhibit and safety).then(logindSuspend(true).fail())

  /// REQ-LOCK-027: the restarted bridge finds the sleep pending, asks again
  /// under a new nonce, and the sleep begins covered.
  run bridgeRestartCoveredSleepTest =
    bridgeCrashPrefix.then(bridgeStart).then(bridgeSeeSleep).then(toLocked)
      .then(compAckLockReq(1)).then(bridgeRecvAck(1)).then(logindSuspend(true))
      .expect(sl.suspended and kms.screen == LockFrame and gh.ackSleep and safety)

  /// Residual risk: the service manager gave the unit up. Covered by
  /// REQ-LOCK-012 like the timer.
  run bridgeUnitLostResidualTest =
    bridgeCrashPrefix.then(bridgeFdLost).then(logindSuspend(true))
      .expect(sl.suspended and kms.screen == SessionFrame and safety)

  /// REQ-LOCK-011: the lock frame cannot be presented. After the retry bound
  /// the compositor switches the output off, acknowledges by the second cause
  /// of REQ-LOCK-002, and the sleep begins with a dark panel.
  run lockDespiteFlipFailureTest =
    init.then(logindPrepareSleep).then(bridgeSeeSleep).then(compLock)
      .then(compRender(true)).then(kmsFlipFail)
      .then(compRender(true)).then(kmsFlipFail)
      .expect(comp.phase == Locking and kms.screen == SessionFrame)
      .then(compBlankOutput).then(compAckNoOutput)
      .then(compAckLockReq(1)).then(bridgeRecvAck(1)).then(logindSuspend(true))
      .expect(sl.suspended and kms.screen == Blank and gh.ackSleep and safety)

  // ----------------------- fresh lock frame before an attempt (REQ-LOCK-015, -010)

  /// The compositor is `Locked` across a sleep it did not acknowledge.
  run lockedAcrossSleepPrefix =
    init.then(toLocked).then(logindPrepareSleep).then(inhibitTimeout(false))
      .then(resume).then(compSeeJump)

  /// REQ-LOCK-015 with `Locked` kept across the resume: a fresh unlock is
  /// authorized before anything is submitted, and the first frame after the
  /// resume is a session frame (15 steps). The first fifteen predicates hold.
  run cx_keep_locked_on_resume =
    lockedAcrossSleepPrefix.then(toVerdict11).then(compRecvVerdict(OK11))
      .then(compRender(true))
      .expect(kms.pending == SessionFrame and coreSafety and not(propB_firstSubmit))

  /// REQ-LOCK-015: after the jump the compositor is `Locking` and opens no
  /// attempt on a presentation from before the sleep.
  run freshLockBeforeAttemptGuardTest =
    lockedAcrossSleepPrefix
      .expect(comp.phase == Locking and kms.screen == LockFrame and safety)
      .then(compOpenAttempt.fail())

  /// REQ-LOCK-015: one frame later the attempt opens and the unlock works.
  run unlockAfterResumeTest =
    lockedAcrossSleepPrefix.then(compRender(true)).then(kmsFlipDone)
      .then(toVerdict11).then(compRecvVerdict(OK11)).then(compRender(true))
      .expect(kms.pending == SessionFrame and gh.sawUnlockAfterResume and safety)

  /// The lock is acknowledged with the output off.
  run darkLockPrefix = init.then(outputOff).then(compLock).then(compAckNoOutput)

  /// REQ-LOCK-010 with attempts allowed on a dark panel: the session unlocks
  /// unseen and the first frame on the returning output is a session frame.
  run cx_blind_attempt =
    darkLockPrefix.then(toVerdict11).then(compRecvVerdict(OK11))
      .then(outputBack).then(compRender(true))
      .expect(kms.pending == SessionFrame and coreSafety and not(propB_firstSubmit))

  /// The same variant: `Locked` on an enabled output without a lock frame.
  run cx_blind_attempt_shown =
    darkLockPrefix.then(outputBack)
      .expect(comp.phase == Locked and kms.screen == Blank and not(propB_shown))

  /// REQ-LOCK-010: no attempt opens while the output is off.
  run noBlindAttemptGuardTest =
    darkLockPrefix.expect(comp.phase == Locked and safety).then(compOpenAttempt.fail())

  /// REQ-LOCK-010: the output returns, the lock frame is presented, and only
  /// then can the user unlock.
  run outputReturnsLockFirstTest =
    darkLockPrefix.then(outputBack)
      .expect(comp.phase == Locking and safety)
      .then(compRender(true)).then(kmsFlipDone)
      .expect(comp.phase == Locked and kms.screen == LockFrame and safety)
      .then(toVerdict11).then(compRecvVerdict(OK11))
      .expect(comp.phase == Unlocked and safety)

  /// REQ-LOCK-010: a verdict that arrives while the output is off is refused.
  run noBlindVerdictGuardTest =
    init.then(toLocked).then(toVerdict11).then(outputOff)
      .then(compRecvVerdict(OK11).fail())

  // ----------------------------------------------- trusted mode (REQ-TRUST-006)

  /// A portal delivery is queued, then a consent prompt for another holder
  /// opens, then the adapter runs the delivery.
  run promptPrefix =
    init.then(toGrant(Portal)).then(compEnqueue(Portal)).then(compAsk(AgentTransfer))
      .then(compDispatch({ h: Portal, gen: 1, seq: 1 }))

  /// Without the rule the frame is delivered while the prompt is open
  /// (6 steps). The first fifteen predicates hold.
  run cx_deliver_during_prompt =
    promptPrefix
      .expect(gh.sawDelivery and comp.ask.open and coreSafety and not(propT_noDelivery))

  /// REQ-TRUST-006: the delivery is dropped; the grant stays live.
  run promptSuppressesDeliveryGuardTest =
    promptPrefix
      .expect(gh.sawSuppressed and not(gh.sawDelivery) and grantLive(Portal) and safety)

  /// REQ-TRUST-006: the reducer emits no delivery while the prompt is open.
  run noEnqueueDuringPromptGuardTest =
    promptPrefix.then(compEnqueue(Portal).fail())

  /// REQ-TRUST-006, REQ-TRUST-012: delivery resumes when the prompt closes.
  /// The prompt closes by timeout, so the pause is bounded.
  run deliveryResumesAfterPromptTest =
    promptPrefix.then(compAskClose).then(compEnqueue(Portal))
      .then(compDispatch({ h: Portal, gen: 1, seq: 2 }))
      .expect(gh.sawDelivery and gh.used == Set(1) and safety)

  /// REQ-TRUST-010: an answer that arrives after the prompt was closed
  /// creates no grant.
  run answerAfterPromptClosedGuardTest =
    init.then(compAsk(Portal)).then(consentAnswer(ASK1, true)).then(compAskClose)
      .then(compRecvAnswer({ id: ASK1, allow: true }).fail())

  // ------------------------------------------ smaller rules and their traces

  /// A wrong and a right secret are typed for attempt 1; authd checks the
  /// wrong one; attempt 2 is opened and its challenge delivered.
  run replayPrefix =
    init.then(toLocked)
      .then(compOpenAttempt).then(authdRecvChallenge(B11))
      .then(userType(false)).then(userType(true))
      .then(authdRecvCred({ b: B11, good: false }))
      .then(compCloseAttempt).then(compOpenAttempt).then(authdRecvChallenge(B12))

  /// A replayed challenge makes binding (1,1,1) judgeable again: pwcheck runs
  /// twice for one binding (13 steps).
  run cx_challenge_replay =
    replayPrefix.then(authdRecvChallenge(B11)).then(authdRecvCred({ b: B11, good: true }))
      .expect(net.checks == Set({ b: B11, good: false }, { b: B11, good: true }))
      .expect(coreSafety and not(propA_onePam))

  /// REQ-AUTH-019: authd refuses a challenge that is not later than its own.
  run challengeReplayGuardTest =
    replayPrefix.then(authdRecvChallenge(B11).fail())

  /// REQ-AUTH-004: the attempt is consumed by whichever of its ending events
  /// the compositor handles first. A valid verdict handled before the exit
  /// notification of lock/ui unlocks (11 steps): the authority is authd's.
  run verdictBeatsUiExitTest =
    init.then(toLocked).then(toVerdict11).then(uiCrash)
      .expect(comp.open and not(auth.uiUp))
      .then(compRecvVerdict(OK11))
      .expect(comp.phase == Unlocked and safety)

  /// REQ-AUTH-004: the exit notification handled first consumes the attempt,
  /// and the verdict that follows is refused.
  run uiExitBeatsVerdictGuardTest =
    init.then(toLocked).then(toVerdict11).then(uiCrash).then(compCloseAttempt)
      .then(compRecvVerdict(OK11).fail())

  /// REQ-GRANT-014: between the reconnect and the compositor's handling of
  /// it the record still says `live`; `grantLive` already says no, and the
  /// disconnect event removes the record.
  run disconnectCleanupTest =
    init.then(toGrant(Portal)).then(holderRestart(Portal))
      .expect(comp.grants.get(Portal).live and not(grantLive(Portal)) and witStaleRecord)
      .then(compSeeDisconnect(Portal))
      .expect(comp.grants.get(Portal) == DEAD and safety)

  /// REQ-GRANT-014: no delivery effect is emitted on a stale record.
  run staleRecordNotEnqueuedGuardTest =
    init.then(toGrant(Portal)).then(holderRestart(Portal))
      .then(compEnqueue(Portal).fail())

  // ------------------------------------- kernel assumption, weakened (variant)

  /// `EnvResettleFlip` only: a lock frame dropped at the first suspend is
  /// shown at the second. `safety` holds either way.
  run envResettleLockFrame =
    init.then(compLock).then(compRender(true))
      .then(logindPrepareSleep).then(inhibitTimeout(false)).then(resume)
      .then(logindPrepareSleep).then(inhibitTimeout(true))
      .expect(kms.resolved and kms.screen == LockFrame and safety)

  /// `EnvResettleFlip` only: a session frame submitted after an authorized
  /// unlock and dropped at the first suspend is shown at the second.
  /// `safety` holds; the exposure is the one REQ-LOCK-012 already states.
  run envResettleSessionFrame =
    init.then(toLocked).then(toVerdict11).then(compRecvVerdict(OK11)).then(compRender(true))
      .then(logindPrepareSleep).then(inhibitTimeout(false)).then(resume)
      .expect(kms.screen == LockFrame and kms.resolved)
      .then(logindPrepareSleep).then(inhibitTimeout(true))
      .expect(kms.screen == SessionFrame and safety)
}
