//! Frame clock, mode timings and vblank throttle, checked through the public
//! API with every time passed in.
//!
//! niri has no tests for `frame_clock.rs` or `vblank_throttle.rs` at the
//! ported revision, so these are new. The intervals are the two modes of the
//! first target panel: 180 Hz (5.555556 ms) and 60 Hz.
//!
//! Property tests run from a fixed seed so that the merge gate gives the same
//! answer on every run.

#![expect(
    clippy::unchecked_time_subtraction,
    reason = "an underflow has to fail the test, which a panic does and saturating would hide"
)]

use std::time::Duration;

use proptest::prelude::*;
use proptest::test_runner::RngSeed;
use skorupa_compositor_core::frame_clock::{
    FrameClock, ModeTimings, VBlankThrottle, refresh_interval,
};

const HZ_60: Duration = Duration::from_nanos(16_666_667);
const HZ_180: Duration = Duration::from_nanos(5_555_556);

const fn ns(nanos: u64) -> Duration {
    Duration::from_nanos(nanos)
}

fn presented_at(refresh: Duration, time: Duration) -> FrameClock {
    let mut clock = FrameClock::new(Some(refresh));
    clock.presented(Some(time));
    clock
}

/// The 2560x1600 timing of the first target panel at the given pixel clock.
fn panel_mode(clock_khz: u32) -> ModeTimings {
    ModeTimings {
        clock_khz,
        htotal: 2720,
        vtotal: 1775,
        vscan: 0,
        interlace: false,
        double_scan: false,
    }
}

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: 2048,
        rng_seed: RngSeed::Fixed(0x534b_4f52_5550_4132),
        ..ProptestConfig::default()
    }
}

#[test]
fn req_perf_003_unknown_refresh_interval_presents_now() {
    let mut clock = FrameClock::new(None);
    clock.presented(Some(ns(1_000)));
    assert_eq!(clock.next_presentation_time(ns(5_000)), ns(5_000));
    assert_eq!(clock.refresh_interval(), None);
}

#[test]
fn req_perf_003_zero_refresh_interval_counts_as_unknown() {
    let clock = FrameClock::new(Some(Duration::ZERO));
    assert_eq!(clock.refresh_interval(), None);
    assert_eq!(clock.next_presentation_time(ns(7)), ns(7));
}

#[test]
fn req_perf_003_before_the_first_presentation_the_target_is_now() {
    let clock = FrameClock::new(Some(HZ_180));
    assert_eq!(clock.next_presentation_time(ns(123_456)), ns(123_456));
}

#[test]
fn req_perf_003_unknown_presentation_time_keeps_the_last_known_one() {
    let mut clock = presented_at(HZ_60, ns(1_000_000_000));
    clock.presented(None);
    assert_eq!(
        clock.next_presentation_time(ns(1_001_000_000)),
        ns(1_016_666_667)
    );
}

#[test]
fn req_perf_003_at_60_hz_the_target_is_the_next_refresh_after_the_last_flip() {
    let clock = presented_at(HZ_60, ns(1_000_000_000));
    assert_eq!(
        clock.next_presentation_time(ns(1_001_000_000)),
        ns(1_016_666_667)
    );
    assert_eq!(
        clock.next_presentation_time(ns(1_016_666_666)),
        ns(1_016_666_667)
    );
}

#[test]
fn req_perf_003_at_180_hz_the_target_is_the_next_refresh_after_the_last_flip() {
    let clock = presented_at(HZ_180, ns(2_000_000_000));
    assert_eq!(
        clock.next_presentation_time(ns(2_001_200_000)),
        ns(2_005_555_556)
    );
}

#[test]
fn req_perf_003_missed_refreshes_are_skipped_on_the_same_grid() {
    let clock = presented_at(HZ_180, ns(2_000_000_000));
    // Two full intervals and a bit have passed: the third multiple is next.
    assert_eq!(
        clock.next_presentation_time(ns(2_012_000_000)),
        ns(2_000_000_000 + 3 * 5_555_556)
    );
    // Exactly on a multiple, that refresh is already being scanned out.
    assert_eq!(
        clock.next_presentation_time(ns(2_000_000_000 + 5_555_556)),
        ns(2_000_000_000 + 2 * 5_555_556)
    );
}

/// The flip event of the target panel arrives 0.24 to 0.33 ms before its own
/// timestamp, so this is the normal path there, not an error path.
#[test]
fn req_perf_003_flip_event_before_its_timestamp_targets_the_refresh_after_it() {
    let last = ns(3_000_000_000);
    let clock = presented_at(HZ_180, last);
    assert_eq!(
        clock.next_presentation_time(last - Duration::from_micros(300)),
        last + HZ_180
    );
    assert_eq!(
        clock.next_presentation_time(last - HZ_180),
        last + HZ_180,
        "a whole interval early is still the same refresh"
    );
}

/// niri treats `now == last` as early and lands two intervals ahead. With a
/// simulated clock that equality is the common case, so it must be exact.
#[test]
fn req_perf_003_flip_event_at_its_timestamp_targets_the_next_refresh() {
    let last = ns(3_000_000_000);
    assert_eq!(
        presented_at(HZ_180, last).next_presentation_time(last),
        last + HZ_180
    );
    assert_eq!(
        presented_at(HZ_60, last).next_presentation_time(last),
        last + HZ_60
    );
}

#[test]
fn req_perf_003_flip_more_than_one_interval_in_the_future_stays_on_the_grid() {
    let last = ns(3_000_000_000);
    let clock = presented_at(HZ_60, last);
    let now = last - 2 * HZ_60 - ns(1);
    assert_eq!(clock.next_presentation_time(now), last + 2 * HZ_60);
}

#[test]
fn req_perf_003_refresh_interval_of_the_180_hz_panel_mode() {
    // 869.04 MHz, htotal 2720, vtotal 1775: measured on the target panel.
    assert_eq!(refresh_interval(panel_mode(869_040)), Some(HZ_180));
}

#[test]
fn req_perf_003_refresh_interval_of_the_60_hz_panel_mode() {
    assert_eq!(refresh_interval(panel_mode(289_680)), Some(HZ_60));
}

#[test]
fn req_perf_003_refresh_interval_honors_interlace_double_scan_and_vscan() {
    let mode = panel_mode(289_680);
    let interlaced = ModeTimings {
        interlace: true,
        ..mode
    };
    assert_eq!(refresh_interval(interlaced), Some(ns(8_333_333)));
    let double_scan = ModeTimings {
        double_scan: true,
        ..mode
    };
    assert_eq!(refresh_interval(double_scan), Some(ns(33_333_333)));
    let vscan = ModeTimings { vscan: 3, ..mode };
    assert_eq!(refresh_interval(vscan), Some(ns(50_000_000)));
    let vscan_one = ModeTimings { vscan: 1, ..mode };
    assert_eq!(refresh_interval(vscan_one), Some(HZ_60));
}

#[test]
fn req_perf_003_mode_without_a_usable_timing_has_no_refresh_interval() {
    assert_eq!(refresh_interval(panel_mode(0)), None);
    let empty = ModeTimings {
        htotal: 0,
        ..panel_mode(869_040)
    };
    assert_eq!(refresh_interval(empty), None);
}

#[test]
fn req_perf_012_relative_throttle_preserves_delay_at_clock_limit() {
    let mut throttle = VBlankThrottle::default();
    assert_eq!(throttle.throttle(Some(HZ_60), Duration::MAX), None);
    assert_eq!(throttle.throttle(Some(HZ_60), Duration::MAX), Some(HZ_60));
}

#[test]
fn req_perf_012_first_vblank_and_unknown_refresh_pass_the_throttle() {
    let mut throttle = VBlankThrottle::default();
    assert_eq!(throttle.throttle(Some(HZ_60), ns(10)), None);
    let mut unknown = VBlankThrottle::default();
    assert_eq!(unknown.throttle(None, ns(10)), None);
    assert_eq!(unknown.throttle(None, ns(11)), None);
}

#[test]
fn req_perf_012_vblank_sooner_than_half_an_interval_is_held_for_the_rest() {
    let mut throttle = VBlankThrottle::default();
    assert_eq!(throttle.throttle(Some(HZ_60), ns(1_000_000_000)), None);
    assert_eq!(
        throttle.throttle(Some(HZ_60), ns(1_001_700_000)),
        Some(HZ_60 - ns(1_700_000))
    );
    // The held vblank did not move the reference point.
    assert_eq!(
        throttle.throttle(Some(HZ_60), ns(1_002_000_000)),
        Some(HZ_60 - ns(2_000_000))
    );
}

#[test]
fn req_perf_012_vblank_at_half_an_interval_or_later_passes() {
    let mut throttle = VBlankThrottle::default();
    assert_eq!(throttle.throttle(Some(HZ_180), ns(1_000_000_000)), None);
    assert_eq!(
        throttle.throttle(Some(HZ_180), ns(1_000_000_000) + HZ_180 / 2),
        None
    );
    assert_eq!(
        throttle.throttle(Some(HZ_180), ns(1_000_000_000) + HZ_180 / 2 + HZ_180),
        None
    );
}

#[test]
fn req_perf_012_timestamp_going_backwards_is_held_a_full_interval() {
    let mut throttle = VBlankThrottle::default();
    assert_eq!(throttle.throttle(Some(HZ_60), ns(1_000_000_000)), None);
    assert_eq!(throttle.throttle(Some(HZ_60), ns(900_000_000)), Some(HZ_60));
}

proptest! {
    #![proptest_config(config())]

    /// With a known grid the target is strictly in the future and a whole
    /// number of intervals after the last flip; once the flip is in the past,
    /// it is at most one interval away.
    #[test]
    fn req_perf_003_target_is_a_future_multiple_of_the_refresh_interval(
        refresh in 1_u64..=1_000_000_000,
        last in 0_u64..=u64::from(u32::MAX),
        now in 0_u64..=2 * u64::from(u32::MAX),
    ) {
        let clock = presented_at(ns(refresh), ns(last));
        let target = clock.next_presentation_time(ns(now));
        prop_assert!(target > ns(now));
        prop_assert!(target > ns(last));
        prop_assert_eq!((target - ns(last)).as_nanos() % u128::from(refresh), 0);
        if now >= last {
            prop_assert!(target - ns(now) <= ns(refresh));
        }
    }

    /// No input makes the clock or the mode formula panic.
    #[test]
    fn req_perf_003_clock_and_mode_formula_accept_any_input(
        refresh in any::<Option<(u64, u32)>>(),
        last in any::<Option<(u64, u32)>>(),
        now in any::<(u64, u32)>(),
        mode in any::<(u32, u16, u16, u16, bool, bool)>(),
    ) {
        let time = |(secs, nanos): (u64, u32)| Duration::new(secs, nanos % 1_000_000_000);
        let mut clock = FrameClock::new(refresh.map(time));
        clock.presented(last.map(time));
        let _ = clock.next_presentation_time(time(now));
        let (clock_khz, htotal, vtotal, vscan, interlace, double_scan) = mode;
        let interval = refresh_interval(ModeTimings {
            clock_khz, htotal, vtotal, vscan, interlace, double_scan,
        });
        prop_assert_ne!(interval, Some(Duration::ZERO));
    }

    /// Vblanks that pass the throttle are at least half an interval apart.
    #[test]
    fn req_perf_012_passed_vblanks_are_at_least_half_an_interval_apart(
        refresh in 2_u64..=100_000_000,
        steps in proptest::collection::vec(0_u64..=200_000_000, 1..64),
    ) {
        let mut throttle = VBlankThrottle::default();
        let mut time = 0_u64;
        let mut last_passed: Option<u64> = None;
        for step in steps {
            time += step;
            match throttle.throttle(Some(ns(refresh)), ns(time)) {
                None => {
                    if let Some(last) = last_passed {
                        prop_assert!(ns(time - last) >= ns(refresh) / 2);
                    }
                    last_passed = Some(time);
                }
                Some(hold) => {
                    prop_assert!(hold <= ns(refresh));
                    prop_assert!(hold > ns(refresh) / 2);
                }
            }
        }
    }
}
