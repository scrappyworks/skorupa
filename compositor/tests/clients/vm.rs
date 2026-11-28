//! A test-only Wayland client for the graphical NixOS VM.

#[allow(dead_code)] // Helpers shared with the socket-pair integration tests.
mod client;

use std::error::Error;
use std::fs;
use std::path::PathBuf;

use client::{Client, Key};
use wayland_client::protocol::wl_shm::Format;

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args().skip(1);
    let digit = arguments.next().ok_or("expected a client number")?;
    let marker = PathBuf::from(arguments.next().ok_or("expected an input marker path")?);
    let ready = arguments.next().map(PathBuf::from);
    let (background, segments) = match digit.as_str() {
        "1" => (0xffc5_2a37, 0b011_0000),
        "2" => (0xff29_3fbc, 0b110_1101),
        _ => return Err("expected client 1 or 2".into()),
    };

    let mut client = Client::from_connection(wayland_client::Connection::connect_to_env()?);
    client.roundtrip()?;
    let window = client.create_window();
    client.roundtrip()?;
    let (width, height) = client.data.configures.last().ok_or("no configure")?.size;
    if !(160..=4096).contains(&width) || !(160..=4096).contains(&height) {
        return Err("unexpected VM window dimensions".into());
    }
    let pixel = |x: i32, y: i32| {
        let x = x - width / 2;
        let y = y - height / 2;
        let rectangles = [
            (-20..20, -56..-48),
            (20..28, -48..-4),
            (20..28, 4..48),
            (-20..20, 48..56),
            (-28..-20, 4..48),
            (-28..-20, -48..-4),
            (-20..20, -4..4),
        ];
        let lit = rectangles.iter().enumerate().any(|(index, (xs, ys))| {
            segments & (1 << (6 - index)) != 0 && xs.contains(&x) && ys.contains(&y)
        });
        if lit { 0xffff_ffff } else { background }
    };
    let buffer = client.buffer_with(width, height, Format::Xrgb8888, pixel);
    client.commit(&window, Some(&buffer));
    let mut feedback_marker = ready.as_ref().map(|path| path.with_extension("feedback"));
    if let Some(path) = ready {
        client.roundtrip()?;
        fs::write(path, [])?;
    }

    let pending = marker.with_extension("pending");
    let mut keys = Vec::new();
    loop {
        if let Some(feedback) = client.data.presented.first()
            && let Some(path) = feedback_marker.take()
        {
            fs::write(path, feedback.flags.to_string())?;
        }
        client.wait()?;
        let previous = keys.len();
        for event in client.data.keys.drain(..) {
            let key = match event {
                Key::Press {
                    key: 30,
                    pressed: true,
                    ..
                } => b'a',
                Key::Press {
                    key: 48,
                    pressed: true,
                    ..
                } => b'b',
                _ => continue,
            };
            if keys.len() == 16 {
                return Err("VM input probe received too many keys".into());
            }
            keys.push(key);
        }
        if previous != keys.len() {
            fs::write(&pending, &keys)?;
            fs::rename(&pending, &marker)?;
        }
    }
}
