//! The headless compositor with real Wayland clients.
//!
//! Compositor and clients run in this one thread, joined by socket pairs,
//! and the test moves the bytes and the clock by hand: no socket in the file
//! system, no sleep, no real time. Every time a client sees comes from the
//! simulated clock.

mod client;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use client::{Client, Key, Presented, Window};
use skorupa_compositor::core::lock::{self, Clocks, Phase};
use skorupa_compositor::protocol::{self, Challenge, LockEpoch, Message, Outcome, Verdict};
use skorupa_compositor::{Compositor, Config};
use wayland_client::protocol::wl_shm::Format;
use wayland_protocols::xdg::shell::client::xdg_toplevel;

/// When the compositor draws its first, empty frame.
const BOOT: Duration = Duration::from_secs(999);
const START: Duration = Duration::from_secs(1000);
const HZ_60: Duration = Duration::from_nanos(16_666_667);
const HZ_180: Duration = Duration::from_nanos(5_555_556);
const WIDTH: i32 = 64;
const HEIGHT: i32 = 48;

const RED: u32 = 0xffff_0000;
const GREEN: u32 = 0xff00_ff00;
const BLUE: u32 = 0xff00_00ff;
const YELLOW: u32 = 0xffff_ff00;

fn config() -> Config {
    Config {
        output_size: (WIDTH, HEIGHT),
        max_buffer_size: (WIDTH, HEIGHT),
        ..Config::default()
    }
}

/// The time of a frame callback as the client sees it.
fn millis(time: Duration) -> u32 {
    u32::try_from(time.as_millis()).unwrap()
}

struct Fixture {
    compositor: Compositor,
    now: Duration,
}

impl Fixture {
    /// A compositor that drew its first, empty frame a second before `START`,
    /// so that the first frame of a client is not held back behind it.
    fn new(config: Config) -> Self {
        let mut compositor = Compositor::new(config).unwrap();
        assert_eq!(compositor.next_deadline(), Some(Duration::ZERO));
        compositor.dispatch(BOOT).unwrap();
        assert_eq!(compositor.next_deadline(), None);
        Self {
            compositor,
            now: START,
        }
    }

    /// A client that has bound the globals and heard their first events.
    fn connect(&mut self) -> Client {
        let (ours, theirs) = UnixStream::pair().unwrap();
        self.compositor.add_client(ours).unwrap();
        let mut client = Client::new(theirs);
        // The registry, then the globals, then what they create (the keyboard).
        for _ in 0..3 {
            self.sync(&mut client);
        }
        client
    }

    /// Moves the clock to `until`, running the compositor at every deadline
    /// on the way and once at the end.
    fn advance_to(&mut self, until: Duration) {
        for _ in 0..10_000 {
            match self.compositor.next_deadline() {
                Some(at) if at <= until => self.now = self.now.max(at),
                _ => {
                    self.now = until;
                    self.compositor.dispatch_at(lock_clocks(until)).unwrap();
                    return;
                }
            }
            self.compositor.dispatch_at(lock_clocks(self.now)).unwrap();
        }
        panic!("the compositor never ran out of deadlines before {until:?}");
    }

    /// Delivers what the client sent, lets the compositor do everything that
    /// is due now, and delivers its answers.
    fn exchange(&mut self, client: &mut Client) {
        client.flush();
        self.advance_to(self.now);
        client.read();
    }

    /// An exchange that is known to be complete.
    fn sync(&mut self, client: &mut Client) {
        let target = client.sync();
        self.exchange(client);
        assert!(client.synced(target), "no answer to wl_display.sync");
    }

    /// A toplevel with a buffer of one opaque color that covers the output.
    fn map(&mut self, client: &mut Client, color: u32) -> Window {
        let window = client.create_window();
        self.sync(client);
        let buffer = client.buffer(WIDTH, HEIGHT, Format::Xrgb8888, color);
        client.commit(&window, Some(&buffer));
        self.sync(client);
        window
    }

    /// Selects a workspace through the same seat input as a physical key.
    fn select_workspace(&mut self, digit: u32) {
        let number = 9 + digit;
        for (key, pressed) in [(133, true), (number, true), (133, false), (number, false)] {
            self.compositor.key(key, pressed, self.now);
        }
    }

    /// The color of the top left pixel of the last frame, without alpha.
    fn color(&mut self) -> u32 {
        let pixels = self.compositor.pixels().unwrap();
        assert_eq!(pixels.len(), usize::try_from(WIDTH * HEIGHT * 4).unwrap());
        u32::from_ne_bytes(*pixels.first_chunk().unwrap()) | 0xff00_0000
    }
}

#[test]
fn req_api_001_client_maps_a_toplevel_that_fills_the_output() {
    let mut fixture = Fixture::new(config());
    let mut client = fixture.connect();

    for global in [
        "wl_compositor",
        "wl_shm",
        "xdg_wm_base",
        "wl_seat",
        "wl_output",
        "wp_presentation",
    ] {
        assert!(
            client.data.globals.iter().any(|name| name == global),
            "{global} is missing from {:?}",
            client.data.globals
        );
    }
    assert_eq!(client.data.mode, Some((WIDTH, HEIGHT, 60_000)));
    // CLOCK_MONOTONIC: the clock the test's times stand for.
    assert_eq!(client.data.clock_id, Some(1));

    let window = client.create_window();
    fixture.sync(&mut client);
    assert_eq!(client.data.configures.len(), 1);
    assert_eq!(client.data.configures[0].size, (WIDTH, HEIGHT));
    assert!(!client.data.configures[0].activated());
    assert_eq!(client.data.keys, [Key::Keymap]);
    assert_eq!(client.data.frames, []);

    let buffer = client.buffer(WIDTH, HEIGHT, Format::Xrgb8888, RED);
    client.commit(&window, Some(&buffer));
    fixture.sync(&mut client);
    assert_eq!(fixture.color(), RED);
    assert!(client.data.on_output);
    assert_eq!(client.data.frames, [millis(START)]);
    assert_eq!(client.data.configures.len(), 2);
    assert!(client.data.configures[1].activated());
    assert_eq!(client.data.keys, [Key::Keymap, Key::Enter]);
    assert!(client.protocol_error().is_none());
}

#[test]
fn req_layout_003_setting_an_unmapped_parent_keeps_the_child_independent() {
    let mut fixture = Fixture::new(config());
    let mut client = fixture.connect();
    let parent = client.create_window();
    fixture.sync(&mut client);
    let child = client.create_toplevel(Some(&parent));
    fixture.sync(&mut client);
    let initial = client.data.configures.last().unwrap();
    assert_eq!(initial.size, (WIDTH, HEIGHT));
    for edge in [
        xdg_toplevel::State::TiledLeft,
        xdg_toplevel::State::TiledRight,
        xdg_toplevel::State::TiledTop,
        xdg_toplevel::State::TiledBottom,
    ] {
        assert!(initial.states.contains(&edge));
    }

    let red = client.buffer(WIDTH, HEIGHT, Format::Xrgb8888, RED);
    client.commit(&parent, Some(&red));
    fixture.sync(&mut client);
    let blue = client.buffer(WIDTH, HEIGHT, Format::Xrgb8888, BLUE);
    client.commit(&child, Some(&blue));
    fixture.sync(&mut client);
    fixture.advance_to(START + HZ_60);
    assert_eq!(fixture.color(), RED);
    fixture.select_workspace(2);
    fixture.advance_to(START + 2 * HZ_60);
    assert_eq!(fixture.color(), BLUE);
}

#[test]
fn req_layout_004_dialog_chooses_its_size_and_is_centered_over_its_parent() {
    let mut fixture = Fixture::new(config());
    let mut client = fixture.connect();
    let parent = fixture.map(&mut client, RED);
    let dialog = client.create_toplevel(Some(&parent));
    fixture.sync(&mut client);
    assert_eq!(client.data.configures.last().unwrap().size, (0, 0));

    let buffer = client.buffer(8, 6, Format::Xrgb8888, BLUE);
    client.commit(&dialog, Some(&buffer));
    fixture.sync(&mut client);
    fixture.advance_to(START + HZ_60);

    let pixels = fixture.compositor.pixels().unwrap();
    for (index, bytes) in pixels.as_chunks::<4>().0.iter().enumerate() {
        let width = usize::try_from(WIDTH).unwrap();
        let (x, y) = (index % width, index / width);
        let inside = (28..36).contains(&x) && (21..27).contains(&y);
        let expected = if inside { BLUE } else { RED };
        assert_eq!(u32::from_ne_bytes(*bytes) | 0xff00_0000, expected);
    }
}

/// A client that commits a new buffer on every frame callback. The headless
/// backend completes a flip at once, so the pacing is the reducer's alone.
fn presents_one_interval_apart(refresh_mhz: i32, interval: Duration) {
    let mut fixture = Fixture::new(Config {
        refresh_mhz,
        ..config()
    });
    let mut client = fixture.connect();
    let window = fixture.map(&mut client, RED);
    let buffers = [
        client.buffer(WIDTH, HEIGHT, Format::Xrgb8888, GREEN),
        client.buffer(WIDTH, HEIGHT, Format::Xrgb8888, BLUE),
    ];

    for frame in 1..20_u32 {
        let expected = START + interval * frame;
        // The callback of the previous frame is already here, although its
        // flip is not: the buffer was latched when the frame was submitted.
        assert_eq!(client.data.frames.len(), usize::try_from(frame).unwrap());
        client.commit(&window, Some(&buffers[usize::try_from(frame % 2).unwrap()]));
        fixture.sync(&mut client);
        assert_eq!(client.data.presented.len(), usize::try_from(frame).unwrap());
        assert_eq!(fixture.compositor.next_deadline(), Some(expected));

        fixture.advance_to(expected);
        client.read();
        assert_eq!(
            client.data.presented.last(),
            Some(&Presented {
                time: expected,
                refresh: interval,
                // The flips before it: the empty first frame and `frame` of ours.
                sequence: u64::from(frame) + 2,
                flags: 0,
            })
        );
        assert_eq!(
            client.data.frames.last(),
            Some(&millis(expected.saturating_sub(interval)))
        );
    }

    assert_eq!(client.data.presented[0].time, START);
    assert_eq!(client.data.discarded, 0);
    // A quiet client leaves nothing to wake up for.
    fixture.advance_to(START + Duration::from_secs(10));
    assert_eq!(fixture.compositor.next_deadline(), None);
    client.read();
    assert_eq!(client.data.presented.len(), 20);
}

#[test]
fn req_perf_015_feedback_carries_the_time_of_each_flip_at_60_hz() {
    presents_one_interval_apart(60_000, HZ_60);
}

#[test]
fn req_perf_012_headless_frames_are_one_interval_apart_at_180_hz() {
    presents_one_interval_apart(180_000, HZ_180);
}

#[test]
fn req_perf_012_time_spent_dispatching_counts_towards_the_frame_deadline() {
    let mut fixture = Fixture::new(config());
    let mut client = fixture.connect();
    let window = fixture.map(&mut client, RED);
    let buffer = client.buffer(WIDTH, HEIGHT, Format::Xrgb8888, BLUE);
    client.commit(&window, Some(&buffer));
    client.flush();

    let draw_cost = Duration::from_millis(5);
    let mut times = [START, START + draw_cost].into_iter();
    let wait = fixture
        .compositor
        .dispatch_timeout(|| times.next().unwrap())
        .unwrap();
    assert_eq!(wait, HZ_60.checked_sub(draw_cost));

    let mut times = [START + draw_cost, START + HZ_60].into_iter();
    let wait = fixture
        .compositor
        .dispatch_timeout(|| times.next().unwrap())
        .unwrap();
    assert_eq!(wait, Some(Duration::ZERO));
}

#[test]
fn req_perf_014_commit_without_damage_waits_for_the_next_refresh_cycle() {
    let mut fixture = Fixture::new(config());
    let mut client = fixture.connect();
    let window = fixture.map(&mut client, RED);
    assert_eq!(client.data.frames, [millis(START)]);

    let commit_time = START + Duration::from_millis(1);
    fixture.advance_to(commit_time);
    for _ in 0..3 {
        client.commit(&window, None);
        fixture.sync(&mut client);
    }
    assert_eq!(client.data.frames.len(), 1, "no callback within the cycle");
    assert_eq!(
        fixture.compositor.next_deadline(),
        Some(commit_time + HZ_60)
    );

    fixture.advance_to(commit_time + HZ_60);
    client.read();
    // All three callbacks come together: they belong to one surface.
    let cycle_end = millis(commit_time + HZ_60);
    assert_eq!(
        client.data.frames,
        [millis(START), cycle_end, cycle_end, cycle_end]
    );
    // Nothing new was shown, so nothing more was presented. Each commit
    // replaced the feedback request of the one before; the last one waits
    // for a frame that shows it.
    assert_eq!(client.data.presented.len(), 1);
    assert_eq!(client.data.discarded, 2);
}

#[test]
fn req_perf_014_covered_client_gets_frame_callbacks_about_a_second_apart() {
    let mut fixture = Fixture::new(config());
    let mut below = fixture.connect();
    let window = fixture.map(&mut below, RED);
    let mut above = fixture.connect();
    fixture.map(&mut above, BLUE);
    fixture.select_workspace(2);
    fixture.advance_to(START + HZ_60);
    assert_eq!(fixture.color(), BLUE);

    // Covered, the client commits again whenever it gets a frame callback.
    let buffer = below.buffer(WIDTH, HEIGHT, Format::Xrgb8888, GREEN);
    for round in 1..=3_usize {
        below.commit(&window, Some(&buffer));
        fixture.sync(&mut below);
        fixture.advance_to(fixture.now + Duration::from_millis(999));
        below.read();
        assert_eq!(
            below.data.frames.len(),
            round,
            "a callback within the second"
        );
        fixture.advance_to(fixture.now + Duration::from_millis(1));
        below.read();
        assert_eq!(
            below.data.frames.len(),
            round + 1,
            "no callback after a second"
        );
    }
    assert_eq!(fixture.color(), BLUE, "the covered buffer was drawn");

    let frames = &below.data.frames;
    assert_eq!(frames[2] - frames[1], 1000, "{frames:?}");
    assert_eq!(frames[3] - frames[2], 1000, "{frames:?}");
    // What was never on screen is not reported as presented: a covered
    // commit is discarded when the next one replaces it.
    assert_eq!(below.data.presented.len(), 1);
    assert_eq!(below.data.discarded, 2);
    fixture.advance_to(fixture.now + Duration::from_secs(10));
    assert_eq!(fixture.compositor.next_deadline(), None);
}

#[test]
fn req_input_002_keys_reach_only_the_focused_client() {
    let mut fixture = Fixture::new(config());
    let mut first = fixture.connect();
    fixture.map(&mut first, RED);
    let mut second = fixture.connect();
    let window = second.create_window();
    fixture.sync(&mut second);
    fixture.sync(&mut first);
    assert_eq!(first.data.keys, [Key::Keymap, Key::Enter]);
    assert_eq!(second.data.keys, [Key::Keymap]);

    let buffer = second.buffer(WIDTH, HEIGHT, Format::Xrgb8888, BLUE);
    second.commit(&window, Some(&buffer));
    fixture.sync(&mut second);
    fixture.sync(&mut first);

    assert_eq!(first.data.keys, [Key::Keymap, Key::Enter]);
    assert_eq!(second.data.keys, [Key::Keymap]);
    fixture.select_workspace(2);
    fixture.sync(&mut first);
    fixture.sync(&mut second);
    assert!(first.data.keys.contains(&Key::Leave));
    assert!(second.data.keys.contains(&Key::Enter));
    assert!(!first.data.configures.last().unwrap().activated());
    assert!(second.data.configures.last().unwrap().activated());
    let first_keys = first.data.keys.len();
    let second_keys = second.data.keys.len();

    // XKB keycode 38 is the evdev key 30 that clients see.
    let time = fixture.now + Duration::from_millis(7);
    fixture.compositor.key(38, true, time);
    fixture.compositor.key(38, false, time);
    fixture.sync(&mut first);
    fixture.sync(&mut second);
    let key = |pressed| Key::Press {
        key: 30,
        pressed,
        time: millis(time),
    };
    assert_eq!(second.data.keys[second_keys..], [key(true), key(false)]);
    assert_eq!(first.data.keys.len(), first_keys);
}

#[test]
fn req_api_011_client_that_disconnects_mid_frame_leaves_the_rest_running() {
    let mut fixture = Fixture::new(config());
    let mut stays = fixture.connect();
    let window = fixture.map(&mut stays, RED);
    let mut leaves = fixture.connect();
    let leaving = fixture.map(&mut leaves, BLUE);
    fixture.select_workspace(2);
    fixture.advance_to(START + HZ_60);

    // A frame of the leaving client is submitted and its flip is pending.
    let buffer = leaves.buffer(WIDTH, HEIGHT, Format::Xrgb8888, GREEN);
    leaves.commit(&leaving, Some(&buffer));
    fixture.sync(&mut leaves);
    assert_eq!(fixture.color(), GREEN);
    let flip = START + 2 * HZ_60;
    assert_eq!(fixture.compositor.next_deadline(), Some(flip));
    drop(leaves);

    // The compositor notices before the flip, then completes it for nobody.
    fixture.advance_to(START + HZ_60 + Duration::from_millis(1));
    fixture.select_workspace(1);
    fixture.advance_to(flip);
    assert_eq!(fixture.color(), RED);
    fixture.sync(&mut stays);
    assert!(stays.data.configures.last().unwrap().activated());
    assert!(stays.data.keys.contains(&Key::Enter));

    // The client that stayed is paced and reported as before.
    fixture.advance_to(flip + HZ_60);
    let buffer = stays.buffer(WIDTH, HEIGHT, Format::Xrgb8888, YELLOW);
    stays.commit(&window, Some(&buffer));
    fixture.sync(&mut stays);
    fixture.advance_to(flip + 2 * HZ_60);
    stays.read();
    assert_eq!(fixture.color(), YELLOW);
    assert_eq!(stays.data.presented.last().unwrap().time, flip + 2 * HZ_60);
    assert!(stays.protocol_error().is_none());
}

#[test]
fn req_api_010_buffer_above_the_limit_is_refused() {
    let mut fixture = Fixture::new(config());
    // Exactly at the limit: accepted.
    let mut modest = fixture.connect();
    let window = fixture.map(&mut modest, RED);
    assert!(modest.protocol_error().is_none());
    assert_eq!(fixture.color(), RED);

    for (width, height) in [(WIDTH + 1, HEIGHT), (WIDTH, HEIGHT + 1)] {
        let mut greedy = fixture.connect();
        let oversized = greedy.create_window();
        fixture.sync(&mut greedy);
        let buffer = greedy.buffer(width, height, Format::Xrgb8888, BLUE);
        greedy.commit(&oversized, Some(&buffer));
        fixture.exchange(&mut greedy);

        let error = greedy.protocol_error().unwrap();
        assert_eq!(error.object_interface, "wl_surface");
        // wl_surface.error.invalid_size
        assert_eq!(error.code, 2);
        fixture.advance_to(fixture.now + HZ_60);
        assert_eq!(fixture.color(), RED, "the refused buffer was drawn");
    }

    // The client within the limit is not disturbed.
    let buffer = modest.buffer(WIDTH, HEIGHT, Format::Xrgb8888, YELLOW);
    modest.commit(&window, Some(&buffer));
    fixture.sync(&mut modest);
    assert_eq!(fixture.color(), YELLOW);
    assert!(modest.data.configures.last().unwrap().activated());
    assert!(modest.protocol_error().is_none());
}

#[test]
fn req_layout_006_workspace_selection_changes_the_visible_client_and_focus() {
    let mut fixture = Fixture::new(config());
    let mut first = fixture.connect();
    let mut second = fixture.connect();
    fixture.map(&mut first, RED);
    fixture.map(&mut second, BLUE);
    fixture.advance_to(START + HZ_60);
    first.read();
    second.read();

    assert_eq!(fixture.color(), RED);
    assert!(first.data.on_output);
    assert!(!second.data.on_output);
    assert!(first.data.configures.last().unwrap().activated());
    assert!(!second.data.configures.last().unwrap().activated());
    assert_eq!(second.data.keys, [Key::Keymap]);
    assert_eq!(second.data.presented, []);

    // XKB keycodes for Left Super and the number 2. Release Super first to
    // exercise suppression of the matching number key release.
    fixture.select_workspace(2);
    fixture.advance_to(START + 2 * HZ_60);
    first.read();
    second.read();

    assert_eq!(fixture.color(), BLUE);
    assert!(!first.data.on_output);
    assert!(second.data.on_output);
    assert!(!first.data.configures.last().unwrap().activated());
    assert!(second.data.configures.last().unwrap().activated());
    assert!(first.data.keys.contains(&Key::Leave));
    assert!(second.data.keys.contains(&Key::Enter));
    assert!(second.data.presented.last().unwrap().time > START);
    for client in [&first, &second] {
        assert!(
            !client
                .data
                .keys
                .iter()
                .any(|event| matches!(event, Key::Press { key: 3, .. }))
        );
    }
}

fn lock_clocks(now: Duration) -> Clocks {
    Clocks::exact(now, now)
}

/// Client requests stay queued while a previous submission delays the cover.
#[test]
fn req_lock_011_pending_cover_precedes_ordinary_wayland_dispatch() {
    let mut fixture = Fixture::new(config());
    let mut client = fixture.connect();
    let window = fixture.map(&mut client, RED);
    fixture.advance_to(START + Duration::from_millis(1));
    let replacement = client.buffer(WIDTH, HEIGHT, Format::Xrgb8888, BLUE);
    client.commit(&window, Some(&replacement));
    fixture.sync(&mut client);

    let (socket, _broker) = UnixStream::pair().unwrap();
    let mut session = [0; protocol::SessionId::MAX_LEN];
    session[0] = b'1';
    let now = lock_clocks(fixture.now);
    fixture
        .compositor
        .enable_session_lock(
            lock::Config {
                instance: lock::InstanceId([1; 16]),
                session: lock::SessionId(session),
                jump_threshold: Duration::from_secs(1),
            },
            now,
            socket,
        )
        .unwrap();
    let request = client.sync();
    client.flush();
    fixture.compositor.dispatch_at(now).unwrap();
    assert_eq!(fixture.compositor.session_phase(), Some(Phase::Locking));
    client.read();
    assert!(!client.synced(request));

    fixture.advance_to(START + 2 * HZ_60);
    client.read();
    assert_eq!(fixture.compositor.session_phase(), Some(Phase::Locked));
    assert!(client.synced(request));
}

#[test]
fn req_auth_001_only_a_current_private_verdict_restores_the_session() {
    let mut fixture = Fixture::new(config());
    let (socket, mut peer) = UnixStream::pair().unwrap();
    peer.set_nonblocking(true).unwrap();
    let mut session = [0; protocol::SessionId::MAX_LEN];
    session[..2].copy_from_slice(b"c2");
    fixture
        .compositor
        .enable_session_lock(
            lock::Config {
                instance: lock::InstanceId([7; 16]),
                session: lock::SessionId(session),
                jump_threshold: Duration::from_secs(1),
            },
            lock_clocks(fixture.now),
            socket,
        )
        .unwrap();
    let mut client = fixture.connect();
    let window = fixture.map(&mut client, RED);
    assert_eq!(fixture.color(), 0xff00_0000);
    assert_eq!(fixture.compositor.session_phase(), Some(Phase::Locked));
    assert_eq!(fixture.compositor.pending_lock_ui(), None);

    let now = lock_clocks(fixture.now);
    let token = fixture.compositor.sleep_hold_token().unwrap();
    assert!(fixture.compositor.end_sleep_hold(now, token));
    let challenge = open_attempt(&mut fixture, &mut peer);
    send_verdict(
        &mut peer,
        Challenge {
            epoch: LockEpoch(challenge.epoch.0 + 1),
            ..challenge
        },
    );
    fixture.advance_to(fixture.now + HZ_60);
    assert_eq!(fixture.compositor.session_phase(), Some(Phase::Locked));
    assert_eq!(fixture.color(), 0xff00_0000);
    fixture
        .compositor
        .key_at(38, true, lock_clocks(fixture.now));
    fixture
        .compositor
        .key_at(38, false, lock_clocks(fixture.now));
    fixture.sync(&mut client);
    assert_eq!(client.data.keys, [Key::Keymap]);

    send_verdict(&mut peer, challenge);
    fixture.advance_to(fixture.now + HZ_60);
    assert_eq!(fixture.compositor.session_phase(), Some(Phase::Unlocked));
    assert_eq!(fixture.color(), RED);
    fixture.sync(&mut client);
    assert_eq!(client.data.keys, [Key::Keymap, Key::Enter]);

    fixture.compositor.request_lock(lock_clocks(fixture.now));
    fixture.advance_to(fixture.now + 2 * HZ_60);
    let current = open_attempt(&mut fixture, &mut peer);
    assert_ne!(challenge.epoch, current.epoch);
    let frames = client.data.frames.len();
    let presentations = client.data.presented.len();
    let buffer = client.buffer(WIDTH, HEIGHT, Format::Xrgb8888, BLUE);
    client.commit(&window, Some(&buffer));
    fixture.sync(&mut client);
    send_verdict(&mut peer, challenge);
    fixture.advance_to(fixture.now + Duration::from_secs(2));
    client.read();
    assert_eq!(fixture.compositor.session_phase(), Some(Phase::Locked));
    assert_eq!(fixture.color(), 0xff00_0000);
    assert_eq!(client.data.frames.len(), frames);
    assert_eq!(client.data.presented.len(), presentations);

    // A late verdict and the current verdict can both be queued before the
    // compositor wakes. Only the latter can restore the session.
    send_verdict(&mut peer, challenge);
    send_verdict(&mut peer, current);
    fixture.advance_to(fixture.now + HZ_60);
    assert_eq!(fixture.compositor.session_phase(), Some(Phase::Locked));
    fixture.advance_to(fixture.now + HZ_60);
    assert_eq!(fixture.compositor.session_phase(), Some(Phase::Unlocked));
    assert_eq!(fixture.color(), BLUE);
}

fn open_attempt(fixture: &mut Fixture, peer: &mut UnixStream) -> Challenge {
    let (ui, ready) = fixture.compositor.pending_lock_ui().unwrap();
    assert!(ready <= fixture.now);
    fixture
        .compositor
        .open_lock_attempt(lock_clocks(fixture.now), ui);
    let mut frame = [0; protocol::MAX_FRAME_LEN];
    peer.read_exact(&mut frame[..protocol::PREFIX_LEN]).unwrap();
    let len = protocol::frame_len(frame[..protocol::PREFIX_LEN].try_into().unwrap()).unwrap();
    peer.read_exact(&mut frame[protocol::PREFIX_LEN..len])
        .unwrap();
    let (Message::Challenge(challenge), _) = protocol::decode(&frame[..len]).unwrap() else {
        panic!("expected a challenge from the compositor");
    };
    challenge
}

fn send_verdict(peer: &mut UnixStream, challenge: Challenge) {
    let message = Message::Verdict(Verdict {
        compositor_instance: challenge.compositor_instance,
        session: challenge.session,
        epoch: challenge.epoch,
        attempt: challenge.attempt,
        outcome: Outcome::Granted,
    });
    let mut frame = [0; protocol::MAX_FRAME_LEN];
    let len = message.encode(&mut frame).unwrap();
    peer.write_all(&frame[..len]).unwrap();
}
