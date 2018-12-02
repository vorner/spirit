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
/// The helpers in this crate ([`TcpListen`](struct.TcpListen.html),
/// [`UdpListen`](struct.UdpListen.html)) use this to make sure they have a runtime to handle the
/// sockets on.
///
/// If you prefer to specify configuration of the runtime to use, instead of the default one, you
/// can create an instance of this helper yourself and register it *before registering any socket
/// helpers*, which will take precedence and the sockets will use the one provided by you.
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
/// TODO: Something with registering it first and registering another helper later on.
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
