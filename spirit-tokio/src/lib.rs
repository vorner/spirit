#![doc(
    html_root_url = "https://docs.rs/spirit-tokio/0.2.0/spirit_tokio/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! A collection of helpers integrating tokio primitives into [spirit]
//!
//! The crate provides few helper implementations that handle listening socket auto-reconfiguration
//! based on configuration.
//!
//! # Examples
//!
//! ```rust
//! extern crate failure;
//! extern crate serde;
//! #[macro_use]
//! extern crate serde_derive;
//! extern crate spirit;
//! extern crate spirit_tokio;
//! extern crate tokio;
//!
//! use std::sync::Arc;
//!
//! use failure::Error;
//! use spirit::{Empty, Spirit};
//! use spirit_tokio::TcpListenWithLimits;
//! use tokio::net::TcpStream;
//! use tokio::prelude::*;
//!
//! const DEFAULT_CONFIG: &str = r#"
//! [listening_socket]
//! port = 1234
//! max-conn = 20
//! error-sleep = "100ms"
//! "#;
//! #[derive(Default, Deserialize)]
//! struct Config {
//!     listening_socket: TcpListenWithLimits,
//! }
//!
//! impl Config {
//!     fn listening_socket(&self) -> TcpListenWithLimits {
//!         self.listening_socket.clone()
//!     }
//! }
//!
//! fn connection(
//!     _: &Arc<Spirit<Empty, Config>>,
//!     _: &Arc<TcpListenWithLimits>,
//!     conn: TcpStream,
//!     _: &str
//! ) -> impl Future<Item = (), Error = Error> {
//!     tokio::io::write_all(conn, "Hello\n")
//!         .map(|_| ())
//!         .map_err(Error::from)
//! }
//!
//! fn main() {
//!     Spirit::<Empty, Config>::new()
//!         .config_defaults(DEFAULT_CONFIG)
//!         .config_helper(
//!             Config::listening_socket,
//!             spirit_tokio::per_connection(connection),
//!             "Listener",
//!         )
//!         .run(|spirit| {
//! #           let spirit = Arc::clone(spirit);
//! #           std::thread::spawn(move || spirit.terminate());
//!             Ok(())
//!         });
//! }
//! ```
//!
//! Further examples are in the
//! [git repository](https://github.com/vorner/spirit/tree/master/spirit-tokio/examples).
//!
//! [spirit]: https://crates.io/crates/spirit.

extern crate failure;
extern crate futures;
#[macro_use]
extern crate log;
extern crate net2;
extern crate parking_lot;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_humanize_rs;
extern crate spirit;
extern crate structopt;
extern crate tk_listen;
extern crate tokio;

pub mod base_traits;
pub mod either;
#[macro_use]
pub mod macros;
pub mod net;
pub mod runtime;
pub mod scaled;
pub mod utils;

pub use base_traits::{ExtraCfgCarrier, Name, ResourceConfig, ResourceConsumer};
pub use net::{TcpListen, TcpListenWithLimits, UdpListen};
pub use runtime::Runtime;
pub use utils::{per_connection, per_connection_init, resource, resources};
