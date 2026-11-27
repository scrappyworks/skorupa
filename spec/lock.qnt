// -*- mode: Bluespec; -*-
//
// Cross-process ordering model for lock, unlock, sleep and grants.
//
// Written before any implementation. It is a proposal for the contract in
// `protocol/`, small enough to simulate in seconds. See README.md in this
// directory for scope, abstractions, commands and the mapping to reducer events.
//
// `DEFECT` selects one deliberately broken variant of the design. The chosen
// design is `DEFECT = "none"`; every other value removes exactly one rule, and
// the simulator is expected to find a counterexample for it (variants.qnt).
module lock {
  const DEFECT: str

  /// Capture of one host window by an agent is planned after v1. With `false`
  /// (v1) the compositor refuses every such request and the only agent grant
  /// is the one-shot clipboard transfer.
  const AGENT_CAPTURE: bool

  pure def defect(d: str): bool = DEFECT == d

  /// A candidate rule that is not part of the chosen design. The only one is
  /// "sleep_hold" (README.md, finding 1).
  pure def option(o: str): bool = DEFECT == o

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

  /// Compositor memory. All of it is lost on restart.
  type Comp = {
    inst: int,
    phase: Phase,
    epoch: int,
    attempt: int,
    open: bool,               // an unlock attempt is open (state "Checking")
    hold: bool,               // option "sleep_hold": no unlock until the sleep is over
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
    asks: Set[AskId],         // compositor -> consent UI
    answers: Set[Answer],     // consent UI -> compositor
    stops: Set[Stop],         // indicator -> compositor
  }

  /// History variables. No process reads them; the invariants do.
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
      if (defect("no_revoke_on_lock")) l else revokeAll(l)
    }

  /// Option "sleep_hold": a new instance does not know whether its predecessor
  /// acknowledged a sleep, so it starts in the hold until the bridge reports.
  pure def freshComp(inst: int): Comp = {
    inst: inst, phase: Locking, epoch: 1, attempt: 0, open: false,
    hold: option("sleep_hold"),
    ask: NO_ASK, askN: 0, grants: HOLDERS.mapBy(_ => DEAD),
  }

  /// Ghost bookkeeping for "the lock is acknowledged".
  pure def ghAck(g: Ghost): Ghost = { ...g, mustCover: true, sawAck: true }

  // -------------------------------------------------------- state predicates
  val awake = not(sl.suspended)

  /// REQ-LOCK-025: the compositor compares CLOCK_BOOTTIME with
  /// CLOCK_MONOTONIC before it handles any other event of a loop iteration.
  val clockOk = awake and (not(sl.jump) or defect("no_boottime"))

  // -------------------------------------------------------------------- init
  action init = all {
    comp' = {
      inst: 1, phase: Unlocked, epoch: 0, attempt: 0, open: false, hold: false,
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
      lockReqs: Set(), lockAcks: Set(), asks: Set(), answers: Set(), stops: Set(),
    },
    gh' = {
      pamOk: Set(), okIssued: Set(), typedGood: Set(),
      badUnlock: false, untypedUnlock: false, staleWakeUnlock: false,
      deadBroker: Set(), brokerUnlock: false, wrongStop: false,
      mustCover: false, openedThisWake: false, mustRelock: false,
      sessionAfterResume: false, lockedSinceReq: false, badRelease: false, ackSleep: false,
      issued: Set(), revoked: Set(), allowed: Set(), unconsented: false,
      late: Set(), used: Set(), reuse: false,
      sawUnlock: false, sawAck: false, sawDelivery: false, sawSleepByAck: false,
      sawNegativeVerdict: false, sawUnlockAfterResume: false, sawDrop: false,
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
    val early = ok and defect("ack_on_submit") and comp.phase == Locking
    val giveUp = not(ok) and defect("unlock_on_render_error") and comp.phase == Locking
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
            g1.sessionAfterResume or (ok and frame == SessionFrame and g1.mustRelock) }
      },
      sl' = sl, auth' = auth, world' = world, net' = net,
    }
  }

  /// The page flip completed: the pending frame is on the panel.
  /// REQ-LOCK-002: the lock is acknowledged here and only when
  /// the completed flip carried a lock frame. A session frame queued before
  /// the lock request may still complete; it acknowledges nothing.
  action kmsFlipDone = {
    val ack = comp.phase == Locking and (kms.pending == LockFrame or defect("ack_any_flip"))
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
      defect("ack_stale_flip") or defect("no_boottime"),
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

  /// The only output is switched off or disappears.
  action outputOff = all {
    awake,
    kms.outputOn,
    kms' = { screen: Blank, busy: false, pending: Blank, outputOn: false, resolved: false },
    comp' = comp, gh' = gh, sl' = sl, auth' = auth, world' = world, net' = net,
  }

  action outputBack = all {
    awake,
    not(kms.outputOn),
    kms' = { ...kms, outputOn: true },
    comp' = comp, gh' = gh, sl' = sl, auth' = auth, world' = world, net' = net,
  }

  /// REQ-LOCK-010: with no enabled output nothing is visible, so the
  /// lock is acknowledged without a frame. The first frame after the output
  /// returns is a lock frame because content follows the phase.
  action compAckNoOutput = all {
    clockOk,
    comp.phase == Locking,
    not(kms.outputOn),
    comp' = { ...comp, phase: Locked },
    gh' = ghAck(gh),
    kms' = kms, sl' = sl, auth' = auth, world' = world, net' = net,
  }

  // ----------------------------------------------- compositor: unlock attempts

  /// REQ-TRUST-002: each attempt gets a number, a fresh lock/ui process and
  /// a fresh credential channel; the challenge tells authd the binding.
  action compOpenAttempt = {
    val b = { ...bindOf(comp), attempt: comp.attempt + 1 }
    all {
      clockOk,
      comp.phase == Locked,
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
    val instOk = v.b.inst == comp.inst or defect("skip_instance_check")
    val epochOk = v.b.epoch == comp.epoch or defect("skip_epoch_check")
    val attemptOk =
      (v.b.attempt == comp.attempt and comp.open) or defect("skip_attempt_check")
    val cur = bindOf(comp)
    val verified = comp.open and gh.okIssued.contains(cur)
    all {
      net.verdicts.contains(v),
      clockOk,
      comp.phase == Locked,
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
    defect("client_unlock"),
    awake,
    comp.phase == Locked,
    comp' = { ...comp, phase: Unlocked, open: false },
    gh' = { ...gh, badUnlock: true },
    kms' = kms, sl' = sl, auth' = auth, world' = world, net' = net,
  }

  /// logind `Unlock`, as requested by `loginctl unlock-session`.
  /// The chosen design ignores it. REQ-LOCK-017.
  action logindUnlockSignal = all {
    defect("honor_logind_unlock"),
    awake,
    comp.phase == Locked,
    comp' = { ...comp, phase: Unlocked, open: false },
    gh' = { ...gh, badUnlock: true },
    kms' = kms, sl' = sl, auth' = auth, world' = world, net' = net,
  }

  /// REQ-LOCK-015, REQ-AUTH-016,
  /// REQ-GRANT-006: a CLOCK_BOOTTIME jump means the machine slept.
  /// The next frame is a lock frame, the open attempt is void, grants are gone.
  action compSeeJump = all {
    awake,
    sl.jump,
    not(defect("no_boottime")),
    sl' = { ...sl, jump: false },
    comp' = {
      val c = { ...revokeAll(lockNow(comp)), hold: false }
      if (defect("keep_attempt_on_resume")) c else { ...c, open: false }
    },
    auth' =
      if (defect("keep_attempt_on_resume")) auth else { ...auth, uiUp: false },
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
    net' = net,
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
  action authdRecvChallenge(b: Bind): bool = all {
    net.challenges.contains(b),
    awake,
    b != auth.ch,
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
    c.b == auth.ch or defect("shared_ui_channel"),
    auth' = { ...auth, used: true },
    net' = { ...net, checks: net.checks.union(Set({ b: auth.ch, good: c.good })) },
    comp' = comp, kms' = kms, sl' = sl, world' = world, gh' = gh,
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
    val keep = defect("keep_attempt_on_authd_disconnect")
    all {
      awake,
      auth.ch != NO_BIND or comp.open,
      auth' = { ...auth, ch: NO_BIND, used: true, uiUp: keep and auth.uiUp },
      comp' = if (keep) comp else { ...comp, open: false },
      gh' = { ...gh, deadBroker: gh.okIssued },
      kms' = kms, sl' = sl, world' = world, net' = net,
    }
  }

  // ------------------------------------------------- logind, bridge, suspend

  action logindPrepareSleep = all {
    awake,
    not(sl.pending),
    sl' = { ...sl, pending: true, cycles: sl.cycles + 1 },
    comp' = comp, kms' = kms, auth' = auth, world' = world, net' = net, gh' = gh,
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
      gh' = { ...gh, lockedSinceReq: false },
      comp' = comp, kms' = kms, auth' = auth, world' = world,
    }
  }

  /// REQ-LOCK-024: the compositor answers a lock request only
  /// while `Locked`, and echoes the nonce of that request.
  /// Option "sleep_hold": the acknowledgement also closes the open attempt
  /// and holds the lock until the sleep is over.
  action compAckLockReq(r: int): bool = all {
    net.lockReqs.contains(r),
    not(net.lockAcks.contains(r)),
    clockOk,
    comp.phase == Locked,
    net' = { ...net, lockAcks: net.lockAcks.union(Set(r)) },
    gh' = { ...gh, lockedSinceReq: gh.lockedSinceReq or r == sl.req },
    comp' = if (option("sleep_hold")) { ...comp, hold: true, open: false } else comp,
    auth' = if (option("sleep_hold")) { ...auth, uiUp: false } else auth,
    kms' = kms, sl' = sl, world' = world,
  }

  /// Option "sleep_hold": the hold ends when the bridge reports that logind
  /// is not preparing for sleep (its `PreparingForSleep` property is false).
  /// After a real sleep the clock jump ends it first, in `compSeeJump`.
  action compHoldRelease = all {
    comp.hold,
    clockOk,
    sl.bridgeUp,
    not(sl.pending),
    comp' = { ...comp, hold: false },
    kms' = kms, sl' = sl, auth' = auth, world' = world, net' = net, gh' = gh,
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
    r == sl.req or defect("stale_ack"),
    sl' = { ...sl, inhibit: false },
    gh' = {
      ...gh,
      badRelease: gh.badRelease or not(gh.lockedSinceReq),
      sawSleepByAck: gh.sawSleepByAck or gh.lockedSinceReq,
      ackSleep: gh.lockedSinceReq,
    },
    comp' = comp, kms' = kms, auth' = auth, world' = world, net' = net,
  }

  /// What the kernel does with a pending page flip when the machine goes to
  /// sleep. Assumption of the model: it settles the flip before sleeping,
  /// either by showing the frame (`drain`) or by dropping it, and the event
  /// reaches the compositor after the resume. No frame submitted before the
  /// sleep reaches the panel after it.
  pure def atSuspend(k: Kms, drain: bool): Kms =
    if (not(k.busy)) k
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

  /// The bridge dies: its inhibitor fd closes with it.
  action bridgeCrash = all {
    awake,
    sl.bridgeUp,
    sl' = { ...sl, bridgeUp: false, inhibit: false, req: 0 },
    comp' = comp, kms' = kms, auth' = auth, world' = world, net' = net, gh' = gh,
  }

  /// A new bridge takes a delay inhibitor; one taken after PrepareForSleep
  /// does not delay the sleep already announced.
  action bridgeStart = all {
    awake,
    not(sl.bridgeUp),
    sl' = { ...sl, bridgeUp: true, inhibit: not(sl.pending), req: 0 },
    comp' = comp, kms' = kms, auth' = auth, world' = world, net' = net, gh' = gh,
  }

  /// Resume. Userspace has not run yet; the panel shows the last frame.
  action resume = all {
    sl.suspended,
    sl' = { ...sl, suspended: false, pending: false, jump: true, inhibit: sl.bridgeUp, req: 0 },
    gh' = { ...gh, mustRelock: true, openedThisWake: false, revoked: gh.issued, ackSleep: false },
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
      a.id.inst == q.id.inst or defect("skip_instance_check"),
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
          unconsented: gh.unconsented or not(gh.allowed.contains(q.id)),
        } else gh,
      kms' = kms, sl' = sl, auth' = auth, net' = net,
    }
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
      g.gen == t.gen or defect("stop_by_holder"),
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
    comp' = if (defect("skip_window_gen")) comp else revokeAll(comp),
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
    world' = { ...world, conn: world.conn.set(h, world.conn.get(h) + 1) },
    gh' = { ...gh, revoked: gh.revoked.union(Set(comp.grants.get(h).gen)) },
    comp' = comp, kms' = kms, sl' = sl, auth' = auth, net' = net,
  }

  /// The reducer decides to deliver (a captured frame, a clipboard transfer)
  /// and emits an effect. The grant is checked here.
  action compEnqueue(h: Holder): bool = all {
    clockOk,
    comp.phase == Unlocked,
    comp.grants.get(h).live,
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
  /// live grant and consumes it.
  action compDispatch(j: Job): bool = {
    val g = comp.grants.get(j.h)
    val valid = defect("check_at_enqueue_only") or and {
      g.live,
      g.gen == j.gen,
      comp.phase == Unlocked,
      g.conn == world.conn.get(j.h) or defect("grant_by_name"),
      not(world.expired.contains(g.gen)) or defect("timer_only_expiry"),
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
          sawDelivery: true,
        } else { ...gh, sawDrop: true },
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
    bridgeSeeSleep,
    compAckLockReq(sl.req),
    bridgeRecvAck(sl.req),
    nondet drain = oneOf(Set(true, false))
    logindSuspend(drain),
    resume,
    compSeeJump,
    compReadStaleFlip,
    compHoldRelease,
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
    all { world.win < MAX_WIN, windowReplaced },
    nondet g = oneOf(gh.issued)
    deadlinePasses(g),
    nondet h = oneOf(HOLDERS)
    all { world.conn.get(h) < MAX_CONN, holderRestart(h) },
    nondet h = oneOf(HOLDERS)
    userPressStop(h),
  }

  /// One step of the whole system. The simulator picks one enabled branch of
  /// `any` uniformly, so repeating a group weights the random walk towards the
  /// plain path (about 1 pick in 12 is a fault, 1 in 12 a stale delivery).
  /// Repetition changes no reachable state.
  action step = any {
    stepLock, stepLock, stepLock, stepLock,
    stepGrant, stepGrant, stepGrant,
    stepSleep, stepSleep,
    stepStart,
    stepStale,
    stepFault,
  }

  /// The same system with every group equally likely: a fault storm.
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
  /// unlock by verdict in the same wake period. The first frame that reaches
  /// the panel after a resume is therefore a lock frame.
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

  val safety = and {
    propA_verdict, propA_pam, propA_typed, propA_wake, propA_broker,
    propB_covered, propB_phase, propB_resume, propS_sleep,
    propC_noLateDelivery, propC_lockRevokes, propC_consent, propC_stopExact,
    propD_agent, propD_v1,
  }

  // ------------------------------------------- stronger, expected to fail
  // Not part of `safety`. The chosen design does not guarantee these; the
  // simulator shows how they fail (README.md, "Findings").

  /// The machine never sleeps with session content on the panel.
  val strongS_sleepCovered = sl.suspended implies kms.screen != SessionFrame

  /// The same, restricted to sleep cycles the compositor acknowledged.
  val strongS_ackedSleepCovered =
    (sl.suspended and gh.ackSleep) implies kms.screen != SessionFrame

  // --------------------------------------------------------------- witnesses
  // Each must be reachable, otherwise the invariants above hold vacuously.
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

  /// Option "sleep_hold": finish the sleep cycle or learn that there is none.
  /// systemd restarts the bridge; logind suspends once the inhibitor is gone.
  action holdProgress =
    if (not(sl.bridgeUp)) bridgeStart
    else if (not(sl.pending)) compHoldRelease
    else if (not(sl.inhibit)) logindSuspend(true)
    else sleepProgress

  action progressStep(wantUnlock: bool): bool =
    if (sl.suspended) resume
    else if (sl.jump) compSeeJump
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
  /// within 20 fault-free steps from any reachable state (32 with the option
  /// "sleep_hold", which may have to finish a sleep cycle first).
  run unlockProgressTest =
    init
      .then(40.reps(_ => step))
      .then((if (option("sleep_hold")) 32 else 20).reps(_ => progressStep(true)))
      .expect(comp.phase == Unlocked and safety)

  // ----------------------------------------------------------- scenario tests

  pure val B11: Bind = { inst: 1, epoch: 1, attempt: 1 }

  pure val B12: Bind = { inst: 1, epoch: 1, attempt: 2 }
  pure val OK11: Result = { b: B11, ok: true }
  pure val ASK1: AskId = { inst: 1, n: 1 }

  /// From `Unlocked` to an acknowledged lock.
  run toLocked = compLock.then(compRender(true)).then(kmsFlipDone)

  /// From a fresh `Locked` to a positive verdict for (1, 1, 1) in flight.
  run toVerdict11 =
    compOpenAttempt
      .then(authdRecvChallenge(B11))
      .then(userType(true))
      .then(authdRecvCred({ b: B11, good: true }))
      .then(pwcheckRun({ b: B11, good: true }))
      .then(authdRecvPam(OK11))

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
      .then(compAsk(Portal))
      .then(consentAnswer({ inst: 1, n: 1 }, true))
      .then(compRecvAnswer({ id: { inst: 1, n: 1 }, allow: true }))
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
      .then(compAsk(AgentTransfer))
      .then(consentAnswer({ inst: 1, n: 1 }, true))
      .then(compRecvAnswer({ id: { inst: 1, n: 1 }, allow: true }))
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

  run instancePrefix =
    init.then(toLocked).then(toVerdict11).then(compRecvVerdict(OK11))
      .then(compRestart).then(compRender(true)).then(kmsFlipDone).then(compOpenAttempt)
      .expect(bindOf(comp) == { inst: 2, epoch: 1, attempt: 1 } and comp.phase == Locked)

  run cx_skip_instance_check =
    instancePrefix.then(compRecvVerdict(OK11))
      .expect(comp.phase == Unlocked and not(propA_verdict))

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

  run resumeAttemptPrefix =
    init.then(toLocked).then(toVerdict11)
      .then(logindPrepareSleep).then(inhibitTimeout(true)).then(resume).then(compSeeJump)

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
      .then(resume).then(compSeeJump)
      .then(toVerdict11).then(compRecvVerdict(OK11))
      .then(logindPrepareSleep).then(bridgeSeeSleep)
      .expect(comp.phase == Unlocked and sl.req == 2 and sl.inhibit)

  run cx_stale_ack =
    staleAckPrefix.then(bridgeRecvAck(1)).then(logindSuspend(true))
      .expect(sl.suspended and comp.phase == Unlocked and not(propS_sleep))

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

  // ------------------------------------------------- known gap, chosen design

  /// Finding 1 in README.md. The sleep acknowledgement is not a commitment:
  /// a valid unlock between the release of the inhibitor and the suspend puts
  /// the machine to sleep with session content on the panel. Every invariant
  /// in `safety` still holds, and the compositor relocks on resume.
  run knownGapUnlockBetweenAckAndSuspendTest =
    init
      .then(logindPrepareSleep).then(bridgeSeeSleep).then(toLocked).then(toVerdict11)
      .then(compAckLockReq(1)).then(bridgeRecvAck(1))
      .then(compRecvVerdict(OK11)).then(compRender(true)).then(kmsFlipDone)
      .then(logindSuspend(true))
      .expect(safety and not(strongS_ackedSleepCovered))
      .then(resume).then(compSeeJump)
      .expect(comp.phase == Locking and kms.screen == SessionFrame and safety)

  /// The same trace with the option "sleep_hold": the acknowledgement closed
  /// the attempt, so the verdict is dropped and the machine sleeps covered.
  run sleepHoldClosesGap =
    init
      .then(logindPrepareSleep).then(bridgeSeeSleep).then(toLocked).then(toVerdict11)
      .then(compAckLockReq(1)).then(bridgeRecvAck(1))
      .expect(comp.hold and not(comp.open))
      .then(compRecvVerdict(OK11).fail())

  /// The first version of the option kept the hold only in memory. The
  /// simulator refuted it: the compositor restarts between its acknowledgement
  /// and the suspend, the new instance knows of no sleep, and a valid unlock
  /// follows. With the hold set at start the new instance opens no attempt.
  run sleepHoldSurvivesRestart =
    init
      .then(logindPrepareSleep).then(bridgeSeeSleep).then(toLocked)
      .then(compAckLockReq(1)).then(compRestart)
      .then(compRender(true)).then(kmsFlipDone).then(bridgeRecvAck(1))
      .expect(comp.phase == Locked and comp.hold and not(sl.inhibit))
      .then(compOpenAttempt.fail())
}
