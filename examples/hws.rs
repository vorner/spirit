//! A hello-world service
//!
//! This is the simpler of the two hello-world-service examples.
//!
//! The service listens on set of TCP sockets. Everyone who connects is greeted with a message and
//! the connection is terminated.
//!
//! Try it out: connect to it, try different logging options on command line. When you start it
//! with a configuration file passed on a command line, you can modify the file and send SIGHUP ‒
//! and see the changes to the logging and the message take effect without restarting the
//! application.

use std::collections::HashSet;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;

use arc_swap::ArcSwap;
use failure::{ensure, Error};
use log::{debug, error, info, warn};
use once_cell::sync::Lazy;
use serde::Deserialize;
use spirit::extension;
use spirit::prelude::*;

// In this part, we define how our configuration looks like. Just like with the `config` crate
// (which is actually used internally), the configuration is loaded using the serde's Deserialize.
//
// Of course the actual structure and names of the configuration is up to the application to
// choose.
//
// The spirit library will further enrich the configuration by logging configuration (and possibly
// other things in the future) and use that internally.

fn default_host() -> String {
    "::".to_owned()
}

/// Description of one listening socket.
#[derive(Clone, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
struct Listen {
    port: u16,
    #[serde(default = "default_host")]
    host: String,
}

/// Description of the user-facing interface.
#[derive(Default, Deserialize)]
struct Ui {
    /// The message printed to each visitor.
    msg: String,
}

#[derive(Default, Deserialize)]
struct Config {
    listen: HashSet<Listen>,
    ui: Ui,
}

/// Here we'll have the current config stored at each time. The spirit library will refresh it
/// with a new version on reload (and the old one will get dropped eventually).
static CONFIG: Lazy<ArcSwap<Config>> = Lazy::new(Default::default);

/// This is used as the base configuration.
const DEFAULT_CONFIG: &str = r#"
[[listen]]
port = 1234

[[listen]]
port = 5678
host = "localhost"

[ui]
msg = "Hello world"
"#;

/// Handles one connection.
///
/// As the configuration is globally accessible, it can directly load the message from there.
fn handle_conn(mut conn: TcpStream) {
    let addr = conn
        .peer_addr()
        .map(|addr| addr.to_string())
        // The address is just for logging, so don't hard-fail on that.
        .unwrap_or_else(|_| "<unknown>".to_owned());
    debug!("Handling connection from {}", addr);
    let msg = format!("{}\n", CONFIG.lease().ui.msg);
    if let Err(e) = conn.write_all(msg.as_bytes()) {
        error!("Failed to handle connection {}: {}", addr, e);
    }
}

/// Start all the threads, one for each listening socket.
fn start_threads() -> Result<(), Error> {
    let config = CONFIG.lease();
    ensure!(!config.listen.is_empty(), "No ports to listen on");
    for listen in &config.listen {
        info!("Starting thread on {}:{}", listen.host, listen.port);
        let listener = TcpListener::bind((&listen.host as &str, listen.port))?;
        thread::spawn(move || {
            for conn in listener.incoming() {
                match conn {
                    Ok(conn) => handle_conn(conn),
                    Err(e) => warn!("Error accepting: {}", e),
                }
            }
        });
    }
    Ok(())
}

fn main() -> Result<(), Error> {
    let (term_send, term_recv) = mpsc::channel();
    let _spirit = Spirit::<spirit::Empty, Config>::new()
        // Keep the current config accessible through a global variable
        .with(spirit_cfg_helpers::cfg_store(&*CONFIG))
        // Set the default config values. This is very similar to passing the first file on command
        // line, except that nobody can lose this one as it is baked into the application. Any
        // files passed by the user can override the values.
        .config_defaults(DEFAULT_CONFIG)
        // If the user passes a directory path instead of file path, take files with these
        // extensions and load config from there.
        .config_exts(&["toml", "ini", "json"])
        // Config can be read from environment too
        .config_env("HWS")
        // Perform some more validation of the results.
        //
        // We are a bit lazy here. Changing the set of ports we listen on at runtime is hard to do.
        // Therefore we simply warn about a change that doesn't take an effect.
        //
        // The hws example in spirit-tokio has a working update of listening ports.
        .with(extension::immutable_cfg(
            |cfg: &Config| &cfg.listen,
            "listen ports",
        ))
        .on_terminate(move || {
            // This unfortunately cuts all the listening threads right away.
            term_send.send(()).unwrap();
        })
        .build(true)?;
    start_threads()?;
    info!("Starting up");
    // And this waits for the ctrl+C or something similar.
    term_recv.recv().unwrap();
    info!("Shutting down");
    Ok(())
}
