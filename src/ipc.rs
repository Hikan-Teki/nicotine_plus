use anyhow::{Context, Result};
use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions, Stream};
use std::io::Write;

const SOCKET_PRINTNAME: &str = "nicotine.sock";

fn socket_name() -> Result<interprocess::local_socket::Name<'static>> {
    SOCKET_PRINTNAME
        .to_ns_name::<GenericNamespaced>()
        .context("Failed to construct named pipe name")
}

pub fn bind_listener() -> Result<interprocess::local_socket::Listener> {
    let name = socket_name()?;
    ListenerOptions::new()
        .name(name)
        .create_sync()
        .context("Failed to bind IPC listener")
}

pub fn send_line(message: &str) -> Result<()> {
    let name = socket_name()?;
    let mut stream =
        Stream::connect(name).context("Failed to connect to nicotine daemon — is it running?")?;
    writeln!(stream, "{}", message)?;
    stream.flush()?;
    Ok(())
}

pub fn daemon_running() -> bool {
    socket_name()
        .and_then(|n| Stream::connect(n).map_err(Into::into))
        .is_ok()
}
