use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use smithay::backend::renderer::sync::{Fence, Interrupted};

use super::*;
use crate::test_support::{connect, exchange};

#[derive(Debug)]
struct TestFence(Arc<AtomicBool>);

impl Fence for TestFence {
    fn is_signaled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    fn wait(&self) -> Result<(), Interrupted> {
        panic!("the event loop must poll a pending fence")
    }

    fn is_exportable(&self) -> bool {
        false
    }

    fn export(&self) -> Option<OwnedFd> {
        None
    }
}

fn prepared(content: Content) -> PreparedFrame {
    PreparedFrame {
        content,
        rendered: frame::RenderedFrame {
            damaged: true,
            ..frame::RenderedFrame::default()
        },
        fence: None,
    }
}

#[test]
fn req_input_005_two_keyboards_share_one_seat_key_transition() {
    use KeyState::{Pressed, Released};

    // The same key goes down on A, down on B, up on A, then up on B.
    let events = [(Pressed, 1), (Pressed, 2), (Released, 1), (Released, 0)];
    let transitions: Vec<_> = events
        .into_iter()
        .filter_map(|(state, count)| seat_key_transition(state, count))
        .collect();
    assert_eq!(transitions, [true, false]);
}

#[test]
fn req_perf_013_lost_flip_allows_session_redraw_while_a_cover_still_needs_its_own_event() {
    let mut frames = DrmFrames {
        pending_flip: Some(PendingFlip::Awaited),
        ..DrmFrames::default()
    };
    assert_eq!(frames.submission(Content::Session), None);
    frames.expire_flip();

    // No event between successive ordinary submissions.
    for _ in 0..3 {
        frames.prepared = Some(prepared(Content::Session));
        frames
            .submit(|mode, content| {
                assert_eq!((mode, content), (Submission::Commit, Content::Session));
                Ok(())
            })
            .unwrap();
        assert_eq!(frames.submission(Content::Protected), None);
    }

    assert!(matches!(
        frames.complete::<&str>(|| panic!("commit_frame already retired the Smithay frame")),
        DrmFlip::Expired
    ));
    frames.prepared = Some(prepared(Content::Protected));
    frames
        .submit(|mode, _| {
            assert_eq!(mode, Submission::Queue);
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        frames.complete(|| Ok::<_, &str>(Some(Content::Protected))),
        DrmFlip::Presented(Content::Protected)
    ));
}

#[test]
fn req_perf_013_failed_synchronous_commit_keeps_the_original_event_pending() {
    let mut frames = DrmFrames {
        pending_flip: Some(PendingFlip::Expired),
        prepared: Some(prepared(Content::Session)),
    };
    assert!(frames.submit(|_, _| Err("commit failed".into())).is_err());
    assert_eq!(frames.submission(Content::Protected), None);
    let mut reaped = false;
    let completion = frames.complete(|| {
        reaped = true;
        Ok::<_, &str>(Some(Content::Session))
    });
    assert!(reaped);
    assert!(matches!(completion, DrmFlip::Expired));
    assert_eq!(
        frames.submission(Content::Protected),
        Some(Submission::Queue)
    );
}

#[test]
fn req_perf_003_failed_submission_retries_only_a_queued_redraw() {
    for queued in [false, true] {
        let mut frames = DrmFrames {
            prepared: Some(prepared(Content::Session)),
            ..DrmFrames::default()
        };
        if queued {
            frames.queue_redraw();
        }
        let Err(failure) = frames.submit(|_, _| Err("commit failed".into())) else {
            panic!("submission unexpectedly succeeded");
        };
        assert_eq!(failure.redraw_needed, queued);
        assert!(frames.prepared.is_none());
    }
}

#[test]
fn req_perf_003_redraws_during_a_fence_wait_preserve_the_frame_until_submission() {
    let signaled = Arc::new(AtomicBool::new(false));
    let mut frames = DrmFrames {
        prepared: Some(PreparedFrame {
            fence: Some(SyncPoint::from(TestFence(signaled.clone()))),
            ..prepared(Content::Session)
        }),
        ..DrmFrames::default()
    };
    for _ in 0..3 {
        frames.queue_redraw();
        assert!(frames.prepared.as_ref().is_some_and(|frame| !frame.ready()));
    }
    signaled.store(true, Ordering::Relaxed);
    assert!(frames.prepared.as_ref().is_some_and(PreparedFrame::ready));
    let rendered = frames
        .submit(|mode, _| {
            assert_eq!(mode, Submission::Queue);
            Ok(())
        })
        .unwrap();
    assert!(rendered.redraw_needed);
}

#[test]
fn req_lock_011_prepared_cover_polls_its_fence_without_waiting() {
    let signaled = Arc::new(AtomicBool::new(false));
    let frame = PreparedFrame {
        fence: Some(SyncPoint::from(TestFence(signaled.clone()))),
        ..prepared(Content::Protected)
    };
    assert!(!frame.ready());
    signaled.store(true, Ordering::Relaxed);
    assert!(frame.ready());
}

fn render_session(state: &mut State) -> frame::RenderedFrame {
    let crate::state::Backend::Headless(backend) = &mut state.backend else {
        unreachable!()
    };
    let elements = frame::elements(
        backend.renderer(),
        &state.output,
        &state.windows,
        &state.placements,
    );
    let (damaged, states) = backend.render(&elements).unwrap();
    frame::RenderedFrame::new(
        &state.output,
        &state.windows,
        Content::Session,
        damaged,
        states,
    )
}

#[test]
fn req_api_001_output_membership_follows_the_prepared_frame_across_a_fence() {
    use wayland_client::protocol::wl_shm;

    let mut compositor = Compositor::new(Config {
        output_size: (4, 4),
        ..Config::default()
    })
    .unwrap();
    let mut client = connect(&mut compositor);
    let window = client.create_window();
    exchange(&mut compositor, &mut client);
    let buffer = client.buffer(4, 4, wl_shm::Format::Xrgb8888, 0xffff_0000);
    client.commit(&window, Some(&buffer));
    exchange(&mut compositor, &mut client);

    let prepared = render_session(&mut compositor.state);
    window.surface.attach(None, 0, 0);
    window.surface.commit();
    exchange(&mut compositor, &mut client);
    compositor
        .state
        .update_primary_scanout_output(&prepared.states);
    exchange(&mut compositor, &mut client);
    assert!(client.data.on_output(&window.surface));

    let next = render_session(&mut compositor.state);
    compositor.state.update_primary_scanout_output(&next.states);
    exchange(&mut compositor, &mut client);
    assert!(!client.data.on_output(&window.surface));
}

#[test]
fn req_perf_015_commit_during_fence_wait_keeps_feedback_for_its_own_frame() {
    use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind;
    use smithay::utils::Monotonic;
    use smithay::wayland::presentation::Refresh;
    use std::time::Duration;
    use wayland_client::protocol::wl_shm;

    let mut compositor = Compositor::new(Config {
        output_size: (4, 4),
        ..Config::default()
    })
    .unwrap();
    let mut client = connect(&mut compositor);
    let window = client.create_window();
    exchange(&mut compositor, &mut client);
    let first = client.buffer(4, 4, wl_shm::Format::Xrgb8888, 0xffff_0000);
    let second = client.buffer(4, 4, wl_shm::Format::Xrgb8888, 0xff00_ff00);
    client.commit(&window, Some(&first));
    exchange(&mut compositor, &mut client);

    let signaled = Arc::new(AtomicBool::new(false));
    let mut frames = DrmFrames {
        prepared: Some(PreparedFrame {
            content: Content::Session,
            rendered: render_session(&mut compositor.state),
            fence: Some(SyncPoint::from(TestFence(signaled.clone()))),
        }),
        ..DrmFrames::default()
    };
    client.commit(&window, Some(&second));
    exchange(&mut compositor, &mut client);
    frames.queue_redraw();
    signaled.store(true, Ordering::Relaxed);
    let first = frames.submit(|_, _| Ok(())).unwrap();
    first.feedback.unwrap().presented::<_, Monotonic>(
        Duration::from_secs(1),
        Refresh::Unknown,
        1,
        Kind::empty(),
    );
    exchange(&mut compositor, &mut client);
    assert_eq!(client.data.discarded, 0);
    assert_eq!(client.data.presented.len(), 1);

    let second = render_session(&mut compositor.state);
    second.feedback.unwrap().presented::<_, Monotonic>(
        Duration::from_secs(2),
        Refresh::Unknown,
        2,
        Kind::empty(),
    );
    exchange(&mut compositor, &mut client);
    assert_eq!(
        client
            .data
            .presented
            .iter()
            .map(|feedback| feedback.sequence)
            .collect::<Vec<_>>(),
        [1, 2]
    );
}

#[test]
fn req_lock_010_only_the_active_seat_rechecks_a_dark_output_immediately() {
    for power in [OutputPower::Off, OutputPower::WakeRequested] {
        assert!(power.off_on_active_seat(true));
        assert!(!power.off_on_active_seat(false));
    }
    assert!(!OutputPower::On.off_on_active_seat(true));
}

#[test]
fn req_perf_003_mode_selection_keeps_raw_refresh_and_skips_invalid_timings() {
    let raw = drm_ffi::drm_mode_modeinfo {
        clock: 148_352,
        hdisplay: 1920,
        vdisplay: 1080,
        htotal: 2200,
        vtotal: 1125,
        ..drm_ffi::drm_mode_modeinfo::default()
    };
    let preferred = ModeTypeFlags::PREFERRED.bits();
    let invalid = DrmMode::from(drm_ffi::drm_mode_modeinfo {
        htotal: 0,
        type_: preferred,
        ..raw
    });
    let usable = DrmMode::from(raw);
    // REQ-LAYOUT-018: a mode preferred by KMS must fit the layout as well.
    for (w, h) in [(16_385, 1080), (1920, 16_385)] {
        let oversized = DrmMode::from(drm_ffi::drm_mode_modeinfo {
            hdisplay: w,
            vdisplay: h,
            htotal: w + 20,
            vtotal: h + 20,
            type_: preferred,
            ..raw
        });
        assert_eq!(select_mode(&[oversized, usable]).unwrap().0, usable);
    }
    assert!(select_mode(&[invalid]).is_none());
    let (chosen, interval) = select_mode(&[invalid, usable]).unwrap();
    assert_eq!(chosen, usable);
    assert_eq!(Mode::from(chosen).refresh, 59_940);
    assert_eq!(interval, Duration::from_nanos(16_683_294));

    let faster = DrmMode::from(drm_ffi::drm_mode_modeinfo {
        clock: 148_500,
        type_: preferred,
        ..raw
    });
    assert_eq!(select_mode(&[usable, faster]).unwrap().0, faster);
}
