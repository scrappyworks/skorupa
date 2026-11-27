// -*- mode: Bluespec; -*-
//
// Instances of the model in lock.qnt. `chosen` is the design. The `rejected_`
// and `ablate_` modules each remove one rule and must produce a counterexample.
// `option_sleep_hold` adds one candidate rule (see README.md).

/// The chosen design for v1: no agent capture of host windows. All invariants
/// must hold.
module chosen { import lock(DEFECT = "none", AGENT_CAPTURE = false).* from "./lock" }

/// Rejected design: the ext-session-lock client unlocks by itself.
module rejected_client_unlock {
  import lock(DEFECT = "client_unlock", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_client_unlock
}

/// The compositor obeys the logind `Unlock` signal.
module ablate_logind_unlock {
  import lock(DEFECT = "honor_logind_unlock", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_honor_logind_unlock
}

/// A failed render of the lock frame gives up the lock (niri's behaviour).
module ablate_render_error {
  import lock(DEFECT = "unlock_on_render_error", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_unlock_on_render_error
}

/// Verdict not bound to the compositor instance.
module ablate_instance {
  import lock(DEFECT = "skip_instance_check", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_skip_instance_check
}

/// Verdict not bound to the lock epoch.
module ablate_epoch {
  import lock(DEFECT = "skip_epoch_check", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_skip_epoch_check
}

/// Verdict not bound to the open attempt.
module ablate_attempt {
  import lock(DEFECT = "skip_attempt_check", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_skip_attempt_check
}

/// One long-lived lock/ui credential channel instead of one per attempt.
module ablate_ui_channel {
  import lock(DEFECT = "shared_ui_channel", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_shared_ui_channel
}

/// Lock acknowledged when the lock frame is submitted, not when it is presented.
module ablate_ack_on_submit {
  import lock(DEFECT = "ack_on_submit", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_ack_on_submit
}

/// Lock acknowledged on the first completed flip, whatever it carried.
module ablate_ack_any_flip {
  import lock(DEFECT = "ack_any_flip", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_ack_any_flip
}

/// No CLOCK_BOOTTIME check; the compositor relies on PrepareForSleep alone.
module ablate_boottime {
  import lock(DEFECT = "no_boottime", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_no_boottime
}

/// The open unlock attempt survives a resume.
module ablate_resume_attempt {
  import lock(DEFECT = "keep_attempt_on_resume", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_keep_attempt_on_resume
}

/// The bridge releases the sleep inhibitor on any lock acknowledgement.
module ablate_stale_ack {
  import lock(DEFECT = "stale_ack", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_stale_ack
}

/// Lock does not revoke grants.
module ablate_lock_revokes {
  import lock(DEFECT = "no_revoke_on_lock", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_no_revoke_on_lock
}

/// The grant is checked when the effect is queued, not when it is executed.
module ablate_dispatch_check {
  import lock(DEFECT = "check_at_enqueue_only", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_check_at_enqueue_only
  run counterexampleAgentTest = cx_check_at_enqueue_only_agent
}

/// The grant is keyed by the holder's name, not by its connection.
module ablate_grant_by_name {
  import lock(DEFECT = "grant_by_name", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_grant_by_name
}

/// Expiry enforced by a timer event only, not by reading the clock at dispatch.
module ablate_timer_expiry {
  import lock(DEFECT = "timer_only_expiry", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_timer_only_expiry
}

/// Grants and prompts not bound to the window generation.
module ablate_window_gen {
  import lock(DEFECT = "skip_window_gen", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_skip_window_gen
}

/// The compositor keeps the open attempt when authd disconnects.
module ablate_broker_disconnect {
  import lock(DEFECT = "keep_attempt_on_authd_disconnect", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_keep_attempt_on_authd_disconnect
}

/// The event of a flip from before the sleep acknowledges the lock.
module ablate_stale_flip {
  import lock(DEFECT = "ack_stale_flip", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_ack_stale_flip
}

/// Stop addressed to the holder instead of the stream generation.
module ablate_stop_generation {
  import lock(DEFECT = "stop_by_holder", AGENT_CAPTURE = false).* from "./lock"
  run counterexampleTest = cx_stop_by_holder
}

/// The chosen design with the post-v1 option: an agent may be granted a
/// one-shot capture of a host window. All invariants must hold here too.
module post_v1_agent_capture {
  import lock(DEFECT = "none", AGENT_CAPTURE = true).* from "./lock"
}

/// Candidate fix for finding 1, not part of the chosen design: after it
/// acknowledges a lock request the compositor accepts no verdict and opens no
/// attempt until the sleep is over; a new instance starts in that hold.
module option_sleep_hold {
  import lock(DEFECT = "sleep_hold", AGENT_CAPTURE = false).* from "./lock"
  run sleepHoldClosesGapTest = sleepHoldClosesGap
  run sleepHoldSurvivesRestartTest = sleepHoldSurvivesRestart
}
