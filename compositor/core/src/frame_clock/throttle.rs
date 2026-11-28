// Ported from niri, https://github.com/niri-wm/niri, GPL-3.0-or-later,
// revision 02ee0585b9da359cb30aa2a13a6023a6e471cc23:
//   src/utils/vblank_throttle.rs
// Changes are listed in docs/niri-ports.md.

use std::time::Duration;

/// Holds back vblanks that come sooner than the output can refresh.
///
/// Some drivers deliver them far too early, and backends without a vblank
/// (virtio-gpu, headless) complete every flip at once. Unthrottled, such an
/// output presents as fast as clients commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VBlankThrottle {
    last_vblank_timestamp: Option<Duration>,
}

impl VBlankThrottle {
    /// `None` lets the vblank through and makes it the new reference point.
    /// `Some(wait)` asks the caller to deliver it again after `wait`.
    pub fn throttle(
        &mut self,
        refresh_interval: Option<Duration>,
        timestamp: Duration,
    ) -> Option<Duration> {
        let deadline = self.deadline(refresh_interval, timestamp)?;
        Some(deadline.saturating_sub(timestamp).min(refresh_interval?))
    }

    /// Absolute pacing deadline on the same monotonic clock as the flip.
    pub(super) fn deadline(
        &mut self,
        refresh_interval: Option<Duration>,
        timestamp: Duration,
    ) -> Option<Duration> {
        if let Some((last, refresh)) = Option::zip(self.last_vblank_timestamp, refresh_interval) {
            let passed = timestamp.saturating_sub(last);
            if passed < refresh / 2 {
                return Some(last.saturating_add(refresh));
            }
        }

        self.last_vblank_timestamp = Some(timestamp);
        None
    }
}
