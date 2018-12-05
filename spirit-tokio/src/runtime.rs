//! A helper to start the tokio runtime at the appropriate time.

use std::fmt::Debug;

use failure::Error;
use futures::future::{self, Future};
use serde::de::DeserializeOwned;
use structopt::StructOpt;
use tokio::runtime;

use spirit::helpers::Helper;
use spirit::Builder;

/// A body run on tokio runtime.
///
/// When specifying custom tokio runtime through the [`Runtime`](enum.Runtime.html) helper, this is
/// the future to be run inside the runtime.
pub type TokioBody = Box<Future<Item = (), Error = Error> + Send>;

/// A helper to initialize a tokio runtime as part of spirit.
///
/// The helpers in this crate ([`TcpListen`], [`UdpListen`]) use this to make sure they have a
/// runtime to handle the sockets on.
///
/// If you prefer to specify configuration of the runtime to use, instead of the default one, you
/// can create an instance of this helper yourself and register it *before registering any socket
/// helpers*, which will take precedence and the sockets will use the one provided by you. You must
/// register it using the [`with_singleton`] method.
///
/// Note that the provided closures are `FnMut` mostly because `Box<FnOnce>` doesn't work. They
/// will be called just once, so you can use `Option<T>` inside and consume the value by
/// `take.unwrap()`.
///
/// # Future compatibility
///
/// More options may be added into the enum at any time. Such change will not be considered a
/// breaking change.
///
/// # Examples
///
/// ```
/// extern crate failure;
/// extern crate serde;
/// #[macro_use]
/// extern crate serde_derive;
/// extern crate spirit;
/// extern crate spirit_tokio;
/// extern crate tokio;
///
/// use std::sync::Arc;
///
/// use failure::Error;
/// use spirit::{Empty, Spirit};
/// use spirit_tokio::TcpListen;
/// use spirit_tokio::net::IntoIncoming;
/// use spirit_tokio::runtime::Runtime;
/// use tokio::net::TcpStream;
/// use tokio::prelude::*;
///
/// #[derive(Default, Deserialize)]
/// struct Config {
///     #[serde(default)]
///     listening_socket: Vec<TcpListen>,
/// }
///
/// impl Config {
///     fn listening_socket(&self) -> Vec<TcpListen> {
///         self.listening_socket.clone()
///     }
/// }
///
/// fn connection<L: IntoIncoming<Connection = TcpStream>>(
///     _spirit: &Arc<Spirit<Empty, Config>>,
///     _resource_config: &Arc<TcpListen>,
///     _listener: L,
///     _name: &str
/// ) -> impl Future<Item = (), Error = Error> {
///     future::ok(()) // Just a dummy implementation
/// }
///
/// fn main() {
///     Spirit::<Empty, Config>::new()
///         // Uses the current thread runtime instead of the default threadpool. This'll create
///         // smaller number of threads.
///         .with_singleton(Runtime::CurrentThread(Box::new(|_| ())))
///         .config_helper(Config::listening_socket, connection, "Listener")
///         .run(|spirit| {
/// #           let spirit = Arc::clone(spirit);
/// #           std::thread::spawn(move || spirit.terminate());
///             Ok(())
///         });
/// }
/// ```
///
/// [`TcpListen`]: ::TcpListen
/// [`UdpListen`]: ::UdpListen
/// [`with_singleton`]: ::spirit::Builder::with_singleton
pub enum Runtime {
    /// Use the threadpool runtime.
    ///
    /// The threadpool runtime is the default (both in tokio and spirit).
    ///
    /// This allows you to modify the builder prior to starting it, specifying custom options.
    ThreadPool(Box<FnMut(&mut runtime::Builder) + Send>),

    /// Use the current thread runtime.
    ///
    /// If you prefer to run everything in a single thread, use this variant. The provided closure
    /// can modify the builder prior to starting it.
    CurrentThread(Box<FnMut(&mut runtime::current_thread::Builder) + Send>),

    /// Use completely custom runtime.
    ///
    /// The provided closure should start the runtime and execute the provided future on it,
    /// blocking until the runtime becomes empty.
    ///
    /// This allows combining arbitrary runtimes that are not directly supported by either tokio or
    /// spirit.
    Custom(Box<FnMut(TokioBody) -> Result<(), Error> + Send>),

    #[doc(hidden)]
    __NonExhaustive__,
    // TODO: Support loading this from configuration? But it won't be possible to modify at
    // runtime, will it?
}

impl Default for Runtime {
    fn default() -> Self {
        Runtime::ThreadPool(Box::new(|_| {}))
    }
}

impl<O, C> Helper<O, C> for Runtime
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
{
    fn apply(self, builder: Builder<O, C>) -> Builder<O, C> {
        trace!("Wrapping in tokio runtime");
        builder.body_wrapper(|_spirit, inner| {
            let fut = future::lazy(move || inner.run());
            match self {
                Runtime::ThreadPool(mut mod_builder) => {
                    let mut builder = runtime::Builder::new();
                    mod_builder(&mut builder);
                    builder.build()?.block_on_all(fut)
                }
                Runtime::CurrentThread(mut mod_builder) => {
                    let mut builder = runtime::current_thread::Builder::new();
                    mod_builder(&mut builder);
                    let mut runtime = builder.build()?;
                    let result = runtime.block_on(fut);
                    runtime.run()?;
                    result
                }
                Runtime::Custom(mut callback) => callback(Box::new(fut)),
                Runtime::__NonExhaustive__ => unreachable!(),
            }
        })
    }
}
