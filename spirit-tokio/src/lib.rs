#![doc(
    html_root_url = "https://docs.rs/spirit-tokio/0.4.1/spirit_tokio/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! A collection of helpers integrating tokio primitives into [spirit].
//!
//! The crate provides utilities to auto-configure tokio primitives, like listening sockets. This
//! allows, for example, specifying just the function that handles a single connection and let
//! spirit handle all the rest ‒ creating and shutting down the listening socket based on the
//! configuration changes, specifying details like the listen backlog or TCP keepalive, etc. It
//! couples them together and spawns the resulting future onto a tokio runtime (also created by
//! this crate, around the main body of application).
//!
//! # Architecture
//!
//! A configuration helper is formed by a pair of traits. They are connected either as a
//! [configuration helper] or with the [`resource`] function (or alternatively [iterated
//! configuration helper] and [`resources`] in case the user is allowed to configure multiple
//! instances).
//!
//! The [`ResourceConfig`] represents the fragment of configuration that describes the resource ‒
//! for example the configuration for TCP listener ([`TcpListen`]). These things can be somewhat
//! customized by type parameters (specifying sub-fragments, adding custom part of configuration
//! used by your application, etc).
//!
//! The [`ResourceConsumer`] is the active part that does something with the resource. This is
//! usually a closure that takes the [`Spirit`] object, the configuration fragment and the resource
//! created from it and returns a future to be spawned onto the runtime. The crate however provides
//! wrappers to build more complex consumers out of simpler closures ‒ for example the
//! [`per_connection`] accepts a closure that handles one connection only, but creates a consumer
//! that handles the whole listener.
//!
//! The [`ResourceConsumer`] may be used multiple times ‒ when the configuration changes, the old
//! future is dropped (canceled) and a new one is created. If the fragment is in a container like
//! [`Vec`] or [`HashSet`], the user is allowed to configure multiple different instances of the
//! resource in parallel and the consumer will be invoked for each of them.
//!
//! Other crates depending on this one may provide their own configs and consumers.
//!
//! The split allows for flexibility and code reuse. A TCP listener is created and configured the
//! same way, no matter what is done with the accepted connections afterwards. So a fragment to
//! configure it can be provided by a crate. On the other hand, the application needs to provide
//! its functionality (handle the connection), but it does not necessarily have to care about if
//! the connections are over TCP socket or unix stream socket or if there's an encryption on top of
//! them.
//!
//! # The sub-fragents
//!
//! Mostly any sub-fragment can be plugged by the [`Empty`]. Such fragment provides the most basic
//! behaviour (usually no action) and brings no additional configuration options.
//!
//! The fragments usually have default sub-fragments so unless you need something special, some
//! sane defaults are provided and the user is given a lot of freedom how to override them.
//!
//! ## Scaling
//!
//! It allows creating multiple instances of the same resource. This allows using multiple threads
//! to handle the same resource. An example may be having too many new connections to accept to be
//! handled in single thread. A consumer is invoked for each instance.
//!
//! It is configured by a sub-fragment implementing the [`Scaled`] trait. The [`Empty`] fragment
//! provides single instance. The [`Scale`] sub-fragment adds the `scale` configuration option and
//! lets the user configure the number of instances.
//!
//! # Available fragments
//!
//! * [`TcpListen`] for [`TcpListener`]
//! * [`UdpListen`] for [`UdpSocket`] (bound to an address)
//! * [`UnixListen`] for [`UnixListener`] (available on unix systems)
//! * [`DatagramListen`] for [`UnixDatagram`] (available on unix systems)
//!
//! The [`WithListenLimits`] is a wrapper that adds limits to number of concurrent connections as
//! well as a backoff timeout in case of soft errors (like „Too many open files“). This allows it
//! to be used with the [`per_connection`] consumer. There are also type aliases
//! [`TcpListenWithLimits`] and [`UnixListenWithLimits`].
//!
//! # Implementing your own
//!
//! To implement your own, you probably want to look at the [source code]. There are, however, few
//! things to note.
//!
//! There are some more traits than the above in play. You may need to implement several. The
//! [`macros`] module contains several helpful macros that can make this part easier.
//!
//! For implementing the „bottom“ resources, you need then [`ResourceConfig`] and probably
//! [`ExtraCfgCarrier`].
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
//! use spirit_tokio::net::limits::LimitedConn;
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
//!     conn: LimitedConn<TcpStream>,
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
//! [configuration helper]: ::spirit::helpers::CfgHelper
//! [iterated conficuration helper]: ::spirit::helpers::IteratedCfgHelper
//! [`Empty`]: ::spirit::Empty
//! [`Spirit`]: ::spirit::Spirit
//! [`Scaled`]: scaled::Scaled
//! [`Scale`]: scaled::Scale
//! [`HashSet`]: ::std::collections::HashSet
//! [source code]: https://github.com/vorner/spirit/tree/master/spirit-tokio/
//! [`TcpListener`]: ::tokio::net::TcpListener
//! [`UdpSocket`]: ::tokio::net::UdpSocket
//! [`UnixListen`]: net::unix::UnixListen
//! [`UnixListener`]: ::tokio::net::unix::UnixListener
//! [`DatagramListen`]: net::unix::DatagramListen
//! [`UnixDatagram`]: ::tokio::net::unix::UnixDatagram
//! [`WithListenLimits`]: net::limits::WithListenLimits
//! [`UnixListenWithLimits`]: net::unix::UnixListenWithLimits

#[cfg(test)]
extern crate corona;
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
