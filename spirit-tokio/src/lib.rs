#![doc(
    html_root_url = "https://docs.rs/spirit-tokio/0.6.1/spirit_tokio/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(
    unknown_lints,
    clippy::unknown_clippy_lints,
    clippy::needless_doctest_main
)]

//! A collection of [`Fragment`]s and [`Extension`]s integrating tokio primitives into [spirit].
//!
//! The crate provides utilities to auto-configure tokio primitives, like listening sockets. This
//! allows, for example, specifying just the function that handles a single connection and let
//! spirit handle all the rest ‒ creating and shutting down the listening socket based on the
//! configuration changes, specifying details like the listen backlog or TCP keepalive, etc. It
//! couples them together and spawns the resulting future onto a tokio runtime (also created by
//! this crate, around the main body of application).
//!
//! # Available fragments
//!
//! * [`TcpListen`] for [`TcpListener`]
//! * [`UdpListen`] for [`UdpSocket`] (bound to an address)
//! * [`UnixListen`] for [`UnixListener`] (available on unix systems)
//! * [`DatagramListen`] for [`UnixDatagram`] (available on unix systems)
//!
//! The [`WithListenLimits`] is a wrapper that adds limits to number of concurrent connections as
//! well as a backoff timeout in case of soft errors (like „Too many open files“). There are also
//! type aliases [`TcpListenWithLimits`] and [`UnixListenWithLimits`].
//!
//! # Runtime
//!
//! Currently, the crate supports spawning futures on the default runtime only. It also sets up the
//! default runtime as part of the pipelines. However, this works only if at least one pipeline is
//! plugged into the [`Builder`]. If all the pipelines you install into [spirit] are plugged after
//! building, it'll panic.
//!
//! In such case, the runtime needs to be plugged in manually as part of the setup, like this:
//!
//! ```rust
//! # use spirit::{Empty, Spirit};
//! # use spirit::prelude::*;
//! # use spirit_tokio::runtime::Runtime;
//! Spirit::<Empty, Empty>::new()
//!     .with_singleton(Runtime::default())
//!     .run(|_spirit| {
//!         // Plugging spirit-tokio pipelines in here into _spirit
//!         Ok(())
//!     });
//! ```
//!
//! # Examples
//!
//! ```rust
//! use std::sync::Arc;
//!
//! use serde::Deserialize;
//! use spirit::{AnyError, Empty, Pipeline, Spirit};
//! use spirit::prelude::*;
//! use spirit_tokio::{HandleListener, TcpListenWithLimits};
//! use tokio::prelude::*;
//!
//! const DEFAULT_CONFIG: &str = r#"
//! [listening_socket]
//! port = 1234
//! max-conn = 20
//! error-sleep = "100ms"
//! "#;
//!
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
//! fn connection<C: AsyncRead + AsyncWrite>(conn: C) -> impl Future<Item = (), Error = AnyError> {
//!     tokio::io::write_all(conn, "Hello\n")
//!         .map(|_| ())
//!         .map_err(AnyError::from)
//! }
//!
//! fn main() {
//!     Spirit::<Empty, Config>::new()
//!         .config_defaults(DEFAULT_CONFIG)
//!         .with(
//!             Pipeline::new("listener")
//!                 .extract_cfg(Config::listening_socket)
//!                 .transform(HandleListener(|conn, _cfg: &_| connection(conn)))
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
//! [`Fragment`]: spirit::Fragment
//! [`Extension`]: spirit::extension::Extension
//! [spirit]: https://crates.io/crates/spirit.
//! [`TcpListener`]: ::tokio::net::TcpListener
//! [`UdpSocket`]: ::tokio::net::UdpSocket
//! [`UnixListen`]: net::unix::UnixListen
//! [`UnixListener`]: ::tokio::net::unix::UnixListener
//! [`DatagramListen`]: net::unix::DatagramListen
//! [`UnixDatagram`]: ::tokio::net::unix::UnixDatagram
//! [`WithListenLimits`]: net::limits::WithListenLimits
//! [`UnixListenWithLimits`]: net::unix::UnixListenWithLimits
//! [`Builder`]: spirit::Builder

pub mod either;
pub mod handlers;
pub mod installer;
pub mod net;
pub mod runtime;
// pub mod scaled; XXX

pub use crate::handlers::{HandleListener, HandleListenerInit, HandleSocket};
pub use crate::net::{TcpListen, TcpListenWithLimits, UdpListen};
pub use crate::runtime::Runtime;
