//! A tokio-based hello world service.
//!
//! Look at hws.rs first, that one is simpler.
//!
//! Unlike that one, it supports reconfiguring of everything ‒ including the ports it listens on.
//! This is currently a bit tricky, something next versions will want to provide some helpers for.
//!
//! # The ports reconfiguration
//!
//! Because we can't know if creating a listening socket will work or not, we have to try it as
//! part of the config validation (with checking if the socket already exists from before). If the
//! configuration fails, the new sockets are dropped. If it succeeds, they are sent to the tokio
//! runtime over a channel and the runtime installs them.
//!
//! The configuration keeps a oneshot channel for each socket. The runtime drops the socket
//! whenever the oneshot fires ‒ which includes when it is dropped. This is used for remotely
//! dropping the sockets. It is used for removing sockets as well as shutting the whole process
//! down (by dropping all of them).
//!
//! There's a small race condition around removing and then re-creating the same socket (think
//! about it). It would be possible to solve, but it would make the code even more complex and the
//! race condition is quite short and unlikely.

#![allow(unused_imports)]
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
use std::sync::Arc;

use failure::Error;
use spirit::helpers;
use spirit_tokio::TcpListen;
use spirit::{Empty, Spirit, SpiritInner};
use tokio::net::TcpStream;
use tokio::prelude::*;
use tokio::reactor::Handle;

// Configuration. It has the same shape as the one in hws.rs.

#[derive(Default, Deserialize)]
struct Ui {
    msg: String,
}

#[derive(Default, Deserialize)]
struct Config {
    listen: HashSet<TcpListen>,
    ui: Ui,
}

impl Config {
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
