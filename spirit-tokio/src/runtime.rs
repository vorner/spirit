//! An extension to start the tokio runtime at the appropriate time.

use std::sync::Arc;

use failure::Error;
use futures::future::{self, Future};
use log::trace;
use serde::de::DeserializeOwned;
use spirit::extension::{Extensible, Extension};
use structopt::StructOpt;
use tokio::runtime;

/// A body run on tokio runtime.
///
/// When specifying custom tokio runtime through the [`Runtime`](enum.Runtime.html) extension, this
/// is the future to be run inside the runtime.
pub type TokioBody = Box<Future<Item = (), Error = Error> + Send>;

/// An extension to initialize a tokio runtime as part of spirit.
///
/// The [`FutureInstaller`] in this crate (and as a result pipelines with [`Fragment`]s like
/// [`TcpListen`], [`UdpListen`]) use this to make sure they have a runtime to handle the sockets
/// on.
///
/// If you prefer to specify configuration of the runtime to use, instead of the default one, you
/// can create an instance of this extension yourself and register it *before registering any socket
/// pipelines*, which will take precedence and the sockets will use the one provided by you. You
/// must register it using the [`with_singleton`] method.
///
/// Similarly, if all the pipelines are registered within the [`run`] method (or generally, after
/// building is done).
///
/// Note that the provided closures are `FnMut` mostly because `Box<FnOnce>` doesn't work. They
/// will be called just once, so you can use `Option<T>` inside and consume the value by
/// `take.unwrap()`.
///
/// # Future compatibility
///
/// More variants may be added into the enum at any time. Such change will not be considered a
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
/// use spirit::prelude::*;
/// use spirit_tokio::{HandleListener, TcpListen};
/// use spirit_tokio::runtime::Runtime;
/// use tokio::prelude::*;
///
/// #[derive(Default, Deserialize)]
/// struct Config {
///     #[serde(default)]
///     listening_socket: Vec<TcpListen>,
/// }
///
/// impl Config {
///     fn listener(&self) -> Vec<TcpListen> {
///         self.listening_socket.clone()
///     }
/// }
///
/// fn connection() -> impl Future<Item = (), Error = Error> {
///     future::ok(()) // Just a dummy implementation
/// }
///
/// fn main() {
///     Spirit::<Empty, Config>::new()
///         // Uses the current thread runtime instead of the default threadpool. This'll create
///         // smaller number of threads.
///         .with_singleton(Runtime::CurrentThread(Box::new(|_| ())))
///         .with(
///             Pipeline::new("listener")
///                 .extract_cfg(Config::listener)
///                 .transform(HandleListener(|_conn, _cfg: &_| connection()))
///         )
///         .run(|spirit| {
/// #           let spirit = Arc::clone(spirit);
/// #           std::thread::spawn(move || spirit.terminate());
///             Ok(())
///         });
/// }
/// ```
///
/// [`TcpListen`]: crate::TcpListen
/// [`UdpListen`]: crate::UdpListen
/// [`FutureInstaller`]: crate::installer::FutureInstaller
/// [`Fragment`]: spirit::Fragment
/// [`run`]: spirit::SpiritBuilder::run
/// [`with_singleton`]: spirit::extension::Extension::with_singleton
pub enum Runtime {
    /// Use the threadpool runtime.
    ///
    /// The threadpool runtime is the default (both in tokio and spirit).
    ///
    /// This allows you to modify the builder prior to starting it, specifying custom options like
    /// number of threads.
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

impl<E> Extension<E> for Runtime
where
    E: Extensible<Ok = E>,
    E::Config: DeserializeOwned + Send + Sync + 'static,
    E::Opts: StructOpt + Send + Sync + 'static,
{
    fn apply(self, ext: E) -> Result<E, Error> {
        trace!("Wrapping in tokio runtime");
        ext.run_around(|spirit, inner| {
            let spirit = Arc::clone(spirit);
            let fut = future::lazy(move || {
                inner.run().map_err(move |e| {
                    spirit.terminate();
                    e
                })
            });
            match self {
                Runtime::ThreadPool(mut mod_builder) => {
                    let mut builder = runtime::Builder::new();
                    mod_builder(&mut builder);
                    let mut runtime = builder.build()?;
                    runtime.block_on(fut)?;
                    runtime.block_on_all(future::lazy(|| Ok(())))
                }
                Runtime::CurrentThread(mut mod_builder) => {
                    let mut builder = runtime::current_thread::Builder::new();
                    mod_builder(&mut builder);
                    let mut runtime = builder.build()?;
                    runtime.block_on(fut)?;
                    runtime.run().map_err(Error::from)
                }
                Runtime::Custom(mut callback) => callback(Box::new(fut)),
                Runtime::__NonExhaustive__ => unreachable!(),
            }
        })
    }
}
