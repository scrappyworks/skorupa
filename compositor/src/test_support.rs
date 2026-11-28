use std::os::unix::net::UnixStream;

use crate::Compositor;

// Share the in-process Wayland client with the headless integration tests.
#[allow(dead_code)]
#[path = "../tests/clients/client.rs"]
pub(crate) mod client;

pub(crate) fn exchange(compositor: &mut Compositor, client: &mut client::Client) {
    client.flush();
    compositor
        .display
        .dispatch_clients(&mut compositor.state)
        .unwrap();
    compositor.display.flush_clients().unwrap();
    client.read();
}

pub(crate) fn connect(compositor: &mut Compositor) -> client::Client {
    let (server, stream) = UnixStream::pair().unwrap();
    compositor.add_client(server).unwrap();
    let mut client = client::Client::new(stream);
    for _ in 0..3 {
        exchange(compositor, &mut client);
    }
    client
}
