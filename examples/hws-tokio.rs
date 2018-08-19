extern crate config;
extern crate failure;
extern crate futures;
#[macro_use]
extern crate log;
extern crate parking_lot;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate spirit;
extern crate tokio;

use std::collections::{HashMap, HashSet};
use std::net::TcpListener as StdTcpListener;
use std::sync::Arc;

use config::FileFormat;
use failure::Error;
use futures::sync::mpsc::{self, Receiver as MReceiver, Sender as MSender};
use futures::sync::oneshot::{self, Receiver, Sender};
use parking_lot::Mutex;
use spirit::validation::Result as ValidationResult;
use spirit::{Empty, Spirit, SpiritInner};
use tokio::net::TcpListener;
use tokio::prelude::*;
use tokio::reactor::Handle;

fn default_host() -> String {
    "::".to_owned()
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
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

const DEFAULT_CONFIG: &str = r#"
[[listen]]
port = 1234

[[listen]]
port = 5678
host = "localhost"

[ui]
msg = "Hello world"
"#;

fn handle_listener(
    spirit: SpiritInner<Empty, Config>,
    listener: TcpListener,
) -> impl Future<Item = (), Error = ()> {
    listener.incoming()
        // FIXME: tk-listen to ignore things like the other side closing connection before we
        // accept
        .for_each(move |conn| {
            let addr = conn.peer_addr()
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
        })
        .map_err(|e| error!("Failed to listen: {}", e))
}

struct NewListener {
    listener: TcpListener,
    terminator: Receiver<()>,
    ack: Sender<()>,
}

fn run(spirit: SpiritInner<Empty, Config>, feeder: MReceiver<NewListener>) -> Result<(), Error> {
    trace!("Running");
    let start_all = feeder.for_each(move |new_listener| {
        let NewListener {
            listener,
            terminator,
            ack,
        } = new_listener;
        let addr = listener
            .local_addr()
            .map(|addr| addr.to_string())
            .unwrap_or_else(|_| "<unknown>".to_owned());
        info!("Listening on {}", addr);
        let terminator = terminator.map_err(move |_| info!("Terminating listener {}", addr));
        let spirit = Arc::clone(&spirit);
        let conn = handle_listener(spirit, listener)
            .select(terminator)
            .then(|_| future::ok(()));
        tokio::spawn(conn);
        trace!("ACKing installation");
        // Can fail if the other side dropped ‒ on startup or on shutdown
        ack.send(()).or_else(|_| Ok(()))
    });
    info!("Starting application");
    tokio::run(start_all);
    info!("Application shut down");
    Ok(())
}

type RemoteStop = Arc<Sender<()>>;
type Listeners = HashMap<Listen, RemoteStop>;

fn listener_config(
    listeners: &Listeners,
    listener: &Listen,
    sender: &MSender<NewListener>,
    ack_wait: &mut Vec<Receiver<()>>,
) -> (Option<RemoteStop>, ValidationResult) {
    if let Some(found) = listeners.get(listener) {
        debug!("Found existing socket for {:?}", listener);
        return (Some(Arc::clone(found)), ValidationResult::nothing());
    }
    debug!("Creating socket for {:?}", listener);
    let socket = StdTcpListener::bind((&listener.host as &str, listener.port))
        .and_then(|socket| TcpListener::from_std(socket, &Handle::default()));
    let socket = match socket {
        Ok(socket) => socket,
        Err(e) => {
            return (
                None,
                ValidationResult::error(format!(
                    "Failed to create listener {}:{}: {}",
                    listener.host, listener.port, e
                )),
            );
        }
    };
    let (stop_send, stop_recv) = oneshot::channel();
    let (ack_send, ack_recv) = oneshot::channel();
    ack_wait.push(ack_recv);
    let sender = sender.clone();
    (
        Some(Arc::new(stop_send)),
        ValidationResult::nothing().on_success(move || {
            trace!("Sending new socket");
            let sent = sender
                .send(NewListener {
                    listener: socket,
                    terminator: stop_recv,
                    ack: ack_send,
                }).wait();
            if sent.is_err() {
                // In case it already shut down
                trace!("Failed to send new socket");
                return;
            }
        }),
    )
}

fn main() {
    let (sender, receiver) = mpsc::channel(10);
    let listeners = Arc::new(Mutex::new(Listeners::new()));
    let listeners_val = Arc::clone(&listeners);
    let sender = Arc::new(Mutex::new(Some(sender)));
    let sender_val = Arc::clone(&sender);
    let mut ack_wait = Vec::<Receiver<()>>::new();
    Spirit::<_, Empty, _>::new(Config::default())
        .config_defaults(DEFAULT_CONFIG, FileFormat::Toml)
        .config_exts(&["toml", "ini", "json"])
        .config_validator(move |_, new_config, _| {
            // Make sure any tcp listeners from the last round are processed
            trace!("Waiting for {} listeners to be processed", ack_wait.len());
            for ack in ack_wait.drain(..) {
                // Ignore errors ‒ they come when the other end shut down or if the config was
                // dropped
                let _ = ack.wait();
            }
            trace!("Wait complete");
            let sender = sender_val.lock();
            let sender = match sender.as_ref() {
                Some(sender) => sender,
                None => return vec![ValidationResult::warning("Already terminated")],
            };
            let listeners = listeners_val.lock();
            let mut new_listeners = Listeners::new();
            let mut results = Vec::with_capacity(new_config.listen.len() + 1);
            for listen in &new_config.listen {
                let (stop, result) = listener_config(&listeners, listen, &sender, &mut ack_wait);
                if let Some(stop) = stop {
                    new_listeners.insert(listen.clone(), stop);
                }
                results.push(result);
            }
            let listeners = Arc::clone(&listeners_val);
            results.push(ValidationResult::nothing().on_success(move || {
                *listeners.lock() = new_listeners;
            }));
            results
        }).on_terminate(move || {
            info!("Terminating");
            sender.lock().take();
            listeners.lock().clear();
        }).run(|spirit| run(spirit, receiver));
}
