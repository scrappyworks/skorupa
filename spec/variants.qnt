// Instances of the model in lock.qnt on which every invariant must hold.
// `chosen` is the design. `post_v1_agent_capture` adds the option planned after
// v1. `env_resettle_flip` keeps every rule and weakens one assumption about the
// kernel. The variants that remove a rule are in ablations.qnt: with all
// instances in one file the flattened model is too large for `quint verify`,
// which then exits with status 0 and prints no verdict.
// `DEFECT` is a sum type, so a misspelt variant name does not typecheck.

/// The chosen design for v1: no agent capture of host windows. All invariants
/// must hold.
module chosen {
  import defects.* from "./lock"
  import lock(DEFECT = NoDefect, AGENT_CAPTURE = false).* from "./lock"
}

/// The chosen design with the post-v1 option: an agent may be granted a
/// one-shot capture of a host window. All invariants must hold here too.
module post_v1_agent_capture {
  import defects.* from "./lock"
  import lock(DEFECT = NoDefect, AGENT_CAPTURE = true).* from "./lock"
}

/// Not an ablation: the chosen design under a weaker assumption about the
/// kernel. A flip dropped at one suspend may be shown at a later one.
/// `safety` must hold; `settledFlipAcrossTwoSleepsTest` does not apply here.
module env_resettle_flip {
  import defects.* from "./lock"
  import lock(DEFECT = EnvResettleFlip, AGENT_CAPTURE = false).* from "./lock"
  run envResettleLockFrameTest = envResettleLockFrame
  run envResettleSessionFrameTest = envResettleSessionFrame
}
