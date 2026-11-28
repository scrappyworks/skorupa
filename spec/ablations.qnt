// Variants of the model in lock.qnt that each remove one rule of the chosen
// design. Every `rejected_` and `ablate_` module must produce a counterexample:
// its `counterexample...Test` runs pass here and fail on `chosen`.
// The instances that must hold are in variants.qnt.

/// Rejected design: the ext-session-lock client unlocks by itself.
module rejected_client_unlock {
  import defects.* from "./lock"
  import lock(DEFECT = ClientUnlock, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_client_unlock
}

/// The compositor obeys the logind `Unlock` signal.
module ablate_logind_unlock {
  import defects.* from "./lock"
  import lock(DEFECT = HonorLogindUnlock, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_honor_logind_unlock
}

/// A failed render of the lock frame gives up the lock (niri's behaviour).
module ablate_render_error {
  import defects.* from "./lock"
  import lock(DEFECT = UnlockOnRenderError, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_unlock_on_render_error
}

/// Verdict not bound to the compositor instance.
module ablate_instance {
  import defects.* from "./lock"
  import lock(DEFECT = SkipInstanceCheck, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_skip_instance_check
  run counterexampleCoincidentTest = cx_coincident_verdict
  run counterexampleConsentTest = cx_coincident_allow
}

/// Verdict not bound to the lock epoch.
module ablate_epoch {
  import defects.* from "./lock"
  import lock(DEFECT = SkipEpochCheck, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_skip_epoch_check
}

/// Verdict not bound to the open attempt.
module ablate_attempt {
  import defects.* from "./lock"
  import lock(DEFECT = SkipAttemptCheck, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_skip_attempt_check
}

/// One long-lived lock/ui credential channel instead of one per attempt.
module ablate_ui_channel {
  import defects.* from "./lock"
  import lock(DEFECT = SharedUiChannel, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_shared_ui_channel
}

/// Lock acknowledged when the lock frame is submitted, not when it is presented.
module ablate_ack_on_submit {
  import defects.* from "./lock"
  import lock(DEFECT = AckOnSubmit, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_ack_on_submit
}

/// Lock acknowledged on the first completed flip, whatever it carried.
module ablate_ack_any_flip {
  import defects.* from "./lock"
  import lock(DEFECT = AckAnyFlip, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_ack_any_flip
}

/// No CLOCK_BOOTTIME check; the compositor relies on PrepareForSleep alone.
module ablate_boottime {
  import defects.* from "./lock"
  import lock(DEFECT = NoBoottime, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_no_boottime
}

/// The open unlock attempt survives a resume.
module ablate_resume_attempt {
  import defects.* from "./lock"
  import lock(DEFECT = KeepAttemptOnResume, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_keep_attempt_on_resume
}

/// The bridge releases the sleep inhibitor on any lock acknowledgement.
module ablate_stale_ack {
  import defects.* from "./lock"
  import lock(DEFECT = StaleAck, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_stale_ack
  run counterexampleCoincidentAckTest = cx_coincident_ack
}

/// Lock does not revoke grants.
module ablate_lock_revokes {
  import defects.* from "./lock"
  import lock(DEFECT = NoRevokeOnLock, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_no_revoke_on_lock
}

/// The grant is checked when the effect is queued, not when it is executed.
module ablate_dispatch_check {
  import defects.* from "./lock"
  import lock(DEFECT = CheckAtEnqueueOnly, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_check_at_enqueue_only
  run counterexampleAgentTest = cx_check_at_enqueue_only_agent
}

/// The grant is keyed by the holder's name, not by its connection.
module ablate_grant_by_name {
  import defects.* from "./lock"
  import lock(DEFECT = GrantByName, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_grant_by_name
}

/// Expiry enforced by a timer event only, not by reading the clock at dispatch.
module ablate_timer_expiry {
  import defects.* from "./lock"
  import lock(DEFECT = TimerOnlyExpiry, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_timer_only_expiry
}

/// Grants and prompts not bound to the window generation.
module ablate_window_gen {
  import defects.* from "./lock"
  import lock(DEFECT = SkipWindowGen, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_skip_window_gen
}

/// The compositor keeps the open attempt when authd disconnects.
module ablate_broker_disconnect {
  import defects.* from "./lock"
  import lock(DEFECT = KeepAttemptOnAuthdDisconnect, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_keep_attempt_on_authd_disconnect
}

/// The event of a flip from before the sleep acknowledges the lock.
module ablate_stale_flip {
  import defects.* from "./lock"
  import lock(DEFECT = AckStaleFlip, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_ack_stale_flip
}

/// Stop addressed to the holder instead of the stream generation.
module ablate_stop_generation {
  import defects.* from "./lock"
  import lock(DEFECT = StopByHolder, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_stop_by_holder
}

/// No sleep hold (the design before REQ-LOCK-026): a verdict is accepted
/// after the compositor acknowledged a lock request for a sleep.
module ablate_sleep_hold {
  import defects.* from "./lock"
  import lock(DEFECT = NoSleepHold, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_no_sleep_hold_release
  run counterexampleSuspendTest = cx_no_sleep_hold_suspend
}

/// The hold lives in compositor memory only; a new instance starts without it.
module ablate_hold_restart {
  import defects.* from "./lock"
  import lock(DEFECT = HoldMemoryOnly, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_hold_memory_only
}

/// The hold ends on any report of the bridge, whatever token it echoes.
module ablate_hold_token {
  import defects.* from "./lock"
  import lock(DEFECT = HoldAnyReport, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_hold_any_report
}

/// The bridge's inhibitor descriptor is not in the service manager's store.
module ablate_fd_store {
  import defects.* from "./lock"
  import lock(DEFECT = NoFdStore, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_no_fd_store
}

/// A compositor that was `Locked` before the sleep stays `Locked` after it.
module ablate_fresh_lock {
  import defects.* from "./lock"
  import lock(DEFECT = KeepLockedOnResume, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_keep_locked_on_resume
}

/// Attempts open and verdicts are accepted while the output is off.
module ablate_shown_lock {
  import defects.* from "./lock"
  import lock(DEFECT = BlindAttempt, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_blind_attempt
  run counterexampleShownTest = cx_blind_attempt_shown
}

/// Delivery to a grant holder continues while a consent prompt is open.
module ablate_trusted_mode {
  import defects.* from "./lock"
  import lock(DEFECT = DeliverDuringPrompt, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_deliver_during_prompt
}

/// authd accepts any challenge that differs from the one it holds.
module ablate_challenge_order {
  import defects.* from "./lock"
  import lock(DEFECT = ChallengeReplay, AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_challenge_replay
}
