extern crate arc_swap;
extern crate config;
#[macro_use]
extern crate failure;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;
#[macro_use]
extern crate serde_derive;
extern crate spirit;

use std::collections::HashSet;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::sync::{mpsc, Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

use arc_swap::ArcSwap;
use config::FileFormat;
use failure::Error;
use spirit::{Spirit, ValidationResult};

fn default_host() -> String {
    "::".to_owned()
}

#[derive(Clone, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
struct Listen {
    port: u16,
    #[serde(default = "default_host")]
    host: String,
}

#[derive(Default, Deserialize)]
struct Ui {
    msg: String,
}

#[derive(Default, Deserialize)]
struct Config {
    listen: HashSet<Listen>,
    ui: Ui,
}

lazy_static! {
    static ref CONFIG: ArcSwap<Config> = ArcSwap::from(Arc::new(Config::default()));
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

fn handle_conn(mut conn: TcpStream) {
    let addr = conn.peer_addr()
        .map(|addr| addr.to_string())
        // The address is just for logging, so don't hard-fail on that.
        .unwrap_or_else(|_| "<unknown>".to_owned());
    debug!("Handling connection from {}", addr);
    let msg = format!("{}\n", CONFIG.lease().ui.msg);
    if let Err(e) = conn.write_all(msg.as_bytes()) {
        error!("Failed to handle connection {}: {}", addr, e);
    }
}

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
    let term_send = Mutex::new(term_send);
    let initial = AtomicBool::new(true);
    let _spirit = Spirit::<_, spirit::Empty, _>::new(&*CONFIG)
        .config_defaults(DEFAULT_CONFIG, FileFormat::Toml)
        .config_validator(move |old, new, _| {
            let mut results = Vec::new();
            if !initial.swap(false, Ordering::Relaxed) && old.listen != new.listen {
                // Sorry, not implemented yet :-(
                results.push(ValidationResult::warning("Can't change listening ports at runtime"))
            }
            results
        })
        .on_terminate(move || {
            // This unfortunately cuts all the listening threads right away.
            term_send.lock().unwrap().send(()).unwrap();
        })
        .build()?;
    start_threads()?;
    info!("Starting up");
    term_recv.recv().unwrap();
    info!("Shutting down");
    Ok(())
}
