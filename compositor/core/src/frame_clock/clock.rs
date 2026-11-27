// Ported from niri, https://github.com/niri-wm/niri, GPL-3.0-or-later,
// revision 02ee0585b9da359cb30aa2a13a6023a6e471cc23:
//   src/frame_clock.rs (FrameClock)
//   src/backend/tty.rs:3101-3123 (refresh_interval)
// Changes are listed in docs/niri-ports.md.

use std::num::NonZeroU64;
use std::time::Duration;

/// Predicts when the next frame can reach the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameClock {
    last_presentation_time: Option<Duration>,
    refresh_interval_ns: Option<NonZeroU64>,
}

impl FrameClock {
    /// `None` is an output without a known refresh rate. So is an interval
    /// of zero or one too long to count in nanoseconds: neither can be a
    /// grid, and treating them as unknown needs no error path.
    #[must_use]
    pub fn new(refresh_interval: Option<Duration>) -> Self {
        let refresh_interval_ns = refresh_interval
            .and_then(|interval| u64::try_from(interval.as_nanos()).ok())
            .and_then(NonZeroU64::new);
        Self {
            last_presentation_time: None,
            refresh_interval_ns,
        }
    }

    #[must_use]
    pub fn refresh_interval(&self) -> Option<Duration> {
        self.refresh_interval_ns
            .map(|interval| Duration::from_nanos(interval.get()))
    }

    /// Records a flip. `None` is a flip whose time is not known; it tells
    /// nothing about the grid, so the last known time stays.
    pub fn presented(&mut self, presentation_time: Option<Duration>) {
        if let Some(time) = presentation_time {
            self.last_presentation_time = Some(time);
        }
    }

    /// The time at which a frame rendered at `now` will be shown.
    #[must_use]
    pub fn next_presentation_time(&self, now: Duration) -> Duration {
        self.predict(now).0
    }

    /// The prediction, and whether the last flip lies more than one refresh
    /// interval in the future, which the caller reports.
    pub(super) fn predict(&self, now: Duration) -> (Duration, bool) {
        let (Some(refresh), Some(last)) = (self.refresh_interval(), self.last_presentation_time)
        else {
            return (now, false);
        };

        let mut now = now;
        let mut far_early = false;
        // The flip event can arrive before the time it carries: on the first
        // target panel it does so by about 0.3 ms on every frame. `now ==
        // last` is not early, unlike in niri, where it lands two intervals
        // ahead; with a simulated clock equality is the common case.
        if now < last {
            now = now.saturating_add(refresh);
            if now < last {
                // niri does not know when this happens either.
                far_early = true;
                now = last.saturating_add(refresh);
            }
        }

        // The next multiple of the interval after `now`, counted from the
        // last flip. A remainder is below the interval, so it fits in u64.
        let since_last = now.saturating_sub(last);
        let into_interval = since_last.as_nanos() % refresh.as_nanos();
        let into_interval = Duration::from_nanos(u64::try_from(into_interval).unwrap_or(0));
        (
            now.saturating_add(refresh.saturating_sub(into_interval)),
            far_early,
        )
    }
}

/// The fields of a KMS mode that fix its refresh rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeTimings {
    /// Pixel clock in kHz.
    pub clock_khz: u32,
    pub htotal: u16,
    pub vtotal: u16,
    /// Times each line is scanned; 0 and 1 both mean once.
    pub vscan: u16,
    pub interlace: bool,
    pub double_scan: bool,
}

/// Refresh interval of a mode, rounded to the nearest nanosecond. `None`
/// when the timings give no rate.
#[must_use]
pub fn refresh_interval(mode: ModeTimings) -> Option<Duration> {
    // u128, because vscan and double scan can push the product past u64.
    let mut numerator = u128::from(mode.htotal) * u128::from(mode.vtotal) * 1_000_000;
    let mut denominator = u128::from(mode.clock_khz);

    if mode.interlace {
        denominator *= 2;
    }
    if mode.double_scan {
        numerator *= 2;
    }
    if mode.vscan > 1 {
        numerator *= u128::from(mode.vscan);
    }

    let interval = (numerator + denominator / 2).checked_div(denominator)?;
    let interval = Duration::from_nanos(u64::try_from(interval).ok()?);
    (!interval.is_zero()).then_some(interval)
}
