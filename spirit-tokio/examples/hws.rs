//! A tokio-based hello world service.
//!
//! Look at hws.rs in core spirit first, that one is simpler.
//!
//! Unlike that one, it supports reconfiguring of everything ‒ including the ports it listens on.
//!
//! # The configuration helpers
//!
//! The port reconfiguration is done by using a helper. By using the provided struct inside the
//! configuration, the helper is able to spawn and shut down tasks inside tokio as needed. You only
//! need to provide it with a function to extract that bit of configuration, the action to take (in
//! case of TCP, the action is handling one incoming connection) and a name (which is used in
//! logs).

extern crate failure;
#[macro_use]
extern crate log;
extern crate parking_lot;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate spirit;
extern crate spirit_tokio;
extern crate tokio;

use std::collections::HashSet;

use failure::Error;
use spirit::{Empty, Spirit, SpiritInner};
use spirit_tokio::TcpListen;
use tokio::net::TcpStream;
use tokio::prelude::*;

// Configuration. It has the same shape as the one in hws.rs.

#[derive(Default, Deserialize)]
struct Ui {
    msg: String,
}

#[derive(Default, Deserialize)]
struct Config {
    /// On which ports (and interfaces) to listen.
    listen: HashSet<TcpListen>,
    /// The UI (there's only the message to send).
    ui: Ui,
}

impl Config {
    /// A function to extract the tcp ports configuration.
    fn listen(&self) -> HashSet<TcpListen> {
        self.listen.clone()
    }
}

const DEFAULT_CONFIG: &str = r#"
[[listen]]
port = 1234

[[listen]]
port = 5678
host = "localhost"

[ui]
msg = "Hello world"
"#;

/// Handle one connection, the tokio way.
fn handle_connection(
    spirit: &SpiritInner<Empty, Config>,
    conn: TcpStream,
    _: &Empty,
) -> impl Future<Item = (), Error = Error> {
    let addr = conn
        .peer_addr()
        .map(|addr| addr.to_string())
        .unwrap_or_else(|_| "<unknown>".to_owned());
    debug!("Handling connection {}", addr);
    let mut msg = spirit.config().ui.msg.clone().into_bytes();
    msg.push(b'\n');
    tokio::io::write_all(conn, msg)
        .map(|_| ()) // Throw away the connection and close it
        .or_else(move |e| {
            warn!("Failed to write message to {}: {}", addr, e);
            future::ok(())
        })
}

pub fn main() {
    Spirit::<_, Empty, _>::new(Config::default())
        .config_defaults(DEFAULT_CONFIG)
        .config_exts(&["toml", "ini", "json"])
        .config_helper(Config::listen, handle_connection, "listen")
        .run(|_| Ok(()));
}
