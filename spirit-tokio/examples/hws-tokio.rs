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

use std::collections::HashSet;
use std::sync::Arc;

use arc_swap::ArcSwap;
use log::{debug, warn};
use serde::Deserialize;
use spirit::prelude::*;
use spirit::{AnyError, Empty, Pipeline, Spirit};
use spirit_tokio::handlers::PerConnection;
use spirit_tokio::net::limits::Tracked;
use spirit_tokio::net::TcpListenWithLimits;
use spirit_tokio::runtime::Cfg as TokioCfg;
use spirit_tokio::Tokio;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

// Configuration. It has the same shape as the one in spirit's hws.rs.

#[derive(Default, Deserialize)]
struct Ui {
    msg: String,
}

#[derive(Default, Deserialize)]
struct Config {
    /// On which ports (and interfaces) to listen.
    listen: HashSet<TcpListenWithLimits>,

    /// The UI (there's only the message to send).
    ui: Ui,

    /// Threadpool to do the async work.
    #[serde(default)]
    threadpool: TokioCfg,
}

impl Config {
    /// A function to extract the tcp ports configuration.
    fn listen(&self) -> &HashSet<TcpListenWithLimits> {
        &self.listen
    }

    /// Extraction of the threadpool configuration
    fn threadpool(&self) -> TokioCfg {
        self.threadpool.clone()
    }
}

const DEFAULT_CONFIG: &str = r#"
[threadpool]
core-threads = 2
max-threads = 4

[[listen]]
port = 1234
max-conn = 30
error-sleep = "50ms"
reuse-addr = true

[[listen]]
port = 5678
host = "127.0.0.1"

[ui]
msg = "Hello world"
"#;

async fn handle_connection(conn: &mut Tracked<TcpStream>, cfg: &Config) -> Result<(), AnyError> {
    let msg = format!("{}\n", cfg.ui.msg);
    conn.write_all(msg.as_bytes()).await?;
    conn.shutdown().await?;
    Ok(())
}

pub fn main() {
    env_logger::init();
    let cfg = Arc::new(ArcSwap::default());
    let cfg_store = Arc::clone(&cfg);
    let conn_handler = move |mut conn: Tracked<TcpStream>, _: &_| {
        let cfg = cfg.load_full();
        async move {
            let addr = conn
                .peer_addr()
                .map(|a| a.to_string())
                .unwrap_or_else(|_| "<unknown>".to_owned());
            debug!("Handling connection {}", addr);
            if let Err(e) = handle_connection(&mut conn, &cfg).await {
                warn!("Failed to handle connection {}: {}", addr, e);
            }
        }
    };
    Spirit::<Empty, Config>::new()
        .on_config(move |_, cfg: &Arc<Config>| cfg_store.store(Arc::clone(&cfg)))
        .config_defaults(DEFAULT_CONFIG)
        .config_exts(&["toml", "ini", "json"])
        .with_singleton(Tokio::from_cfg(Config::threadpool))
        .with(
            Pipeline::new("listen")
                .extract_cfg(Config::listen)
                .transform(PerConnection(conn_handler)),
        )
        .run(|_| Ok(()));
}
