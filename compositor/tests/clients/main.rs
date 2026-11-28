//! The headless compositor with real Wayland clients.
//!
//! Compositor and clients run in this one thread, joined by socket pairs,
//! and the test moves the bytes and the clock by hand: no socket in the file
//! system, no sleep, no real time. Every time a client sees comes from the
//! simulated clock.

mod client;

use std::os::unix::net::UnixStream;
use std::time::Duration;

use client::{Client, Key, Presented, Window};
use skorupa_compositor::{Compositor, Config};
use wayland_client::protocol::wl_shm::Format;

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
                    self.compositor.dispatch(until).unwrap();
                    return;
                }
            }
            self.compositor.dispatch(self.now).unwrap();
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

    // The newest mapped toplevel has the focus; the older one was told so.
    assert_eq!(first.data.keys, [Key::Keymap, Key::Enter, Key::Leave]);
    assert_eq!(second.data.keys, [Key::Keymap, Key::Enter]);
    assert_eq!(first.data.configures.len(), 3);
    assert!(!first.data.configures[2].activated());
    assert!(second.data.configures[1].activated());

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
    assert_eq!(second.data.keys[2..], [key(true), key(false)]);
    assert_eq!(first.data.keys.len(), 3);
}

#[test]
fn req_api_011_client_that_disconnects_mid_frame_leaves_the_rest_running() {
    let mut fixture = Fixture::new(config());
    let mut stays = fixture.connect();
    let window = fixture.map(&mut stays, RED);
    let mut leaves = fixture.connect();
    let leaving = fixture.map(&mut leaves, BLUE);
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
    fixture.advance_to(flip);
    assert_eq!(fixture.color(), RED);
    fixture.sync(&mut stays);
    assert!(stays.data.configures.last().unwrap().activated());
    assert_eq!(stays.data.keys.last(), Some(&Key::Enter));

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
