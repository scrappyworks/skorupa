use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use smithay::backend::renderer::sync::{Fence, Interrupted};

use super::*;

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

// Share the in-process Wayland client with the headless integration tests.
#[allow(dead_code)]
#[path = "../../tests/clients/client.rs"]
mod client;

fn exchange(compositor: &mut Compositor, client: &mut client::Client) {
    client.flush();
    compositor
        .display
        .dispatch_clients(&mut compositor.state)
        .unwrap();
    compositor.display.flush_clients().unwrap();
    client.read();
}

fn render_session(state: &mut State) -> frame::RenderedFrame {
    let crate::state::Backend::Headless(backend) = &mut state.backend else {
        unreachable!()
    };
    let elements = frame::elements(backend.renderer(), &state.windows, &state.placements);
    let (damaged, states) = backend.render(&elements).unwrap();
    frame::RenderedFrame::new(
        &state.output,
        &state.windows,
        Content::Session,
        damaged,
        states,
    )
}

fn present_session_frame(
    state: &mut State,
    rendered: frame::RenderedFrame,
    stamp: Duration,
    sequence: u64,
) {
    use skorupa_compositor_core::frame_clock::{Event, RenderOutcome};
    // Mirror the queued submission before exercising the KMS completion and
    // the feedback that the client receives over its real Wayland connection.
    let mut pacer = state.pacer.clone();
    for event in [
        Event::QueueRedraw,
        Event::Redraw { now: stamp },
        Event::Rendered {
            now: stamp,
            outcome: RenderOutcome::Submitted,
            animations_remain: false,
        },
    ] {
        pacer = pacer.step(event).0;
    }
    state.pacer = pacer;
    state.pending_feedback = rendered.feedback;
    state.presented_drm(
        stamp + Duration::from_millis(1),
        Some(stamp),
        sequence,
        Content::Session,
    );
}

#[test]
fn req_perf_015_commit_during_fence_wait_keeps_feedback_for_its_own_frame() {
    use std::os::unix::net::UnixStream;
    use std::time::Duration;
    use wayland_client::protocol::wl_shm;

    let mut compositor = Compositor::new(Config {
        output_size: (4, 4),
        ..Config::default()
    })
    .unwrap();
    let (server, stream) = UnixStream::pair().unwrap();
    compositor.add_client(server).unwrap();
    let mut client = client::Client::new(stream);
    for _ in 0..3 {
        exchange(&mut compositor, &mut client);
    }
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
    let first_stamp = Duration::from_millis(999);
    let mut vblank = CrtcSequence::default();
    present_session_frame(
        &mut compositor.state,
        first,
        first_stamp,
        vblank.observe(Some(u32::MAX)),
    );
    exchange(&mut compositor, &mut client);
    assert_eq!(client.data.discarded, 0);
    assert_eq!(client.data.presented.len(), 1);

    let second = render_session(&mut compositor.state);
    let second_stamp = Duration::from_millis(1999);
    present_session_frame(
        &mut compositor.state,
        second,
        second_stamp,
        vblank.observe(Some(0)),
    );
    exchange(&mut compositor, &mut client);
    assert_eq!(
        client
            .data
            .presented
            .iter()
            .map(|feedback| (feedback.sequence, feedback.time, feedback.flags))
            .collect::<Vec<_>>(),
        [
            (u64::from(u32::MAX), first_stamp, 0),
            (1 << 32, second_stamp, 0)
        ]
    );
}

#[test]
fn req_perf_012_virtual_connector_uses_software_pacing_even_with_a_kernel_stamp() {
    use connector::Interface::{EmbeddedDisplayPort, Virtual};
    let stamp = Duration::from_secs(1);
    assert_eq!(
        presentation_sample(Virtual, Some(DrmEventTime::Monotonic(stamp))),
        None
    );
    assert_eq!(
        presentation_sample(EmbeddedDisplayPort, Some(DrmEventTime::Monotonic(stamp))),
        Some(stamp)
    );
    assert_eq!(
        presentation_sample(
            EmbeddedDisplayPort,
            Some(DrmEventTime::Realtime(std::time::UNIX_EPOCH))
        ),
        None
    );
    assert_eq!(presentation_sample(EmbeddedDisplayPort, None), None);
}

#[test]
fn req_perf_015_missing_flip_metadata_preserves_the_vblank_counter() {
    let mut vblank = CrtcSequence::default();
    let sequences = [
        Some(u32::MAX - 1),
        None,
        Some(u32::MAX),
        Some(0),
        None,
        Some(0),
        Some(2),
    ];
    let observed = sequences.map(|low| vblank.observe(low));
    assert_eq!(
        observed,
        [
            u64::from(u32::MAX - 1),
            0,
            u64::from(u32::MAX),
            1 << 32,
            0,
            1 << 32,
            (1 << 32) + 2
        ]
    );
}
