//! An extension to start the tokio runtime at the appropriate time.

/*
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use futures::future::{self, Future};
use log::{trace, warn};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use spirit::extension::{Extensible, Extension};
use spirit::AnyError;
use spirit::{Builder, Spirit};
use structdoc::StructDoc;
use tokio::runtime;
*/

use std::sync::{Arc, Mutex, PoisonError};

use err_context::AnyError;
use err_context::prelude::*;
use log::{debug, trace, warn};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use spirit::extension::{Extensible, Extension};
use spirit::validation::Action;
use structopt::StructOpt;
use tokio::runtime::{Builder, Runtime};

// TODO: Make rt-threaded optional (which'd disable a lot of stuff in here I guess?)

// From tokio documentation
const DEFAULT_MAX_THREADS: usize = 512;
const THREAD_NAME: &str = "tokio-runtime-worker";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct Cfg {
    #[serde(skip_serializing_if = "Option::is_none")]
    core_threads: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_threads: Option<usize>,
    thread_name: String,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg {
            core_threads: None,
            max_threads: None,
            thread_name: THREAD_NAME.to_owned(),
        }
    }
}

impl Into<Builder> for Cfg {
    fn into(self) -> Builder {
        let mut builder = Builder::new();
        let threads = self.core_threads.unwrap_or_else(num_cpus::get);
        builder.core_threads(threads);
        let max = match self.max_threads {
            None if threads >= DEFAULT_MAX_THREADS => {
                warn!(
                    "Increasing max threads from implicit {} to {} to match core threads",
                    DEFAULT_MAX_THREADS, threads
                );
                threads
            }
            None => DEFAULT_MAX_THREADS,
            Some(max_threads) if max_threads < threads => {
                warn!(
                    "Incrementing max threads from configured {} to {} to match core threads",
                    max_threads, threads
                );
                threads
            }
            Some(max) => max,
        };
        builder.max_threads(max);
        builder.thread_name(self.thread_name);
        builder
    }
}

#[non_exhaustive]
pub enum Tokio<O, C> {
    Default,
    Custom(Box<dyn FnMut(&O, &Arc<C>) -> Result<Runtime, AnyError> + Send>),
    FromCfg(
        Box<dyn FnMut(&O, &C) -> Cfg + Send>,
        Box<dyn FnMut(Builder) -> Result<Runtime, AnyError> + Send>,
    ),
}

impl<O, C> Tokio<O, C> {
    pub fn create(&mut self, opts: &O, cfg: &Arc<C>) -> Result<Runtime, AnyError> {
        match self {
            Tokio::Default => Runtime::new().map_err(AnyError::from),
            Tokio::Custom(ctor) => ctor(opts, cfg),
            Tokio::FromCfg(extractor, postprocess) => {
                let cfg = extractor(opts, cfg);
                let mut builder: Builder = cfg.into();
                builder.enable_all().threaded_scheduler();
                postprocess(builder)
            }
        }
    }
}

impl<O, C> Default for Tokio<O, C> {
    fn default() -> Self {
        Tokio::Default
    }
}

impl<E> Extension<E> for Tokio<E::Opts, E::Config>
where
    E: Extensible<Ok = E>,
    E::Config: DeserializeOwned + Send + Sync + 'static,
    E::Opts: StructOpt + Send + Sync + 'static,
{
    fn apply(mut self, ext: E) -> Result<E, AnyError> {
        trace!("Wrapping in tokio runtime");

        // The local hackonomicon:
        //
        // * We need to create a runtime and wrap both the application body in it and the
        //   callbacks/hooks of spirit, as they might want to create some futures/resources that
        //   need access to tokio.
        // * But that will be ready only after we did the configuration. By that time we no longer
        //   have access to anything that would allow us this kind of injection.
        // * Furthermore, we need even the following on_config callbacks/whatever else to be
        //   wrapped in the handle.
        //
        // Therefore we install the wrappers in right away and provide them with the data only once
        // it is ready. The mutexes will always be unlocked, all these things will in fact be
        // called from the same thread, it's just not possible to explain to Rust right now.

        let runtime = Arc::new(Mutex::new(None));
        let handle = Arc::new(Mutex::new(None));

        let init = {
            let mut initialized = false;
            let runtime = Arc::clone(&runtime);
            let handle = Arc::clone(&handle);
            let mut prev_cfg = None;

            // This runs as config validator to be sooner in the chain.
            move |_: &_, cfg: &Arc<_>, opts: &_| -> Result<Action, AnyError> {
                if initialized {
                    if let Tokio::FromCfg(extract, _) = &mut self {
                        let prev = prev_cfg.as_ref().expect("Should have stored config on init");
                        let new = extract(opts, &cfg);
                        if prev != &new {
                            warn!("Tokio configuration differs, but can't be reloaded at run time");
                        }
                    }
                } else {
                    debug!("Creating the tokio runtime");
                    let new_runtime = self.create(opts, &cfg).context("Tokio runtime creation")?;
                    let new_handle = new_runtime.handle().clone();
                    // We do so *right now* so the following config validators have some chance of
                    // having the runtime. It's one-shot anyway, so we are not *replacing* anything
                    // previous.
                    *runtime.lock().unwrap() = Some(new_runtime);
                    *handle.lock().unwrap() = Some(new_handle);
                    initialized = true;
                    if let Tokio::FromCfg(extract, _) = &mut self {
                        prev_cfg = Some(extract(opts, &cfg));
                    }
                }
                Ok(Action::new())
            }
        };

        // Ugly hack: seems like Rust is not exactly ready to deal with our crazy type in here and
        // deduce it in just the function call :-(. Force it by an explicit type.
        let around_hooks: Box<dyn for<'a> FnMut(Box<dyn FnOnce() + 'a>) + Send> =
            Box::new(move |inner| {
                let locked = handle
                    .lock()
                    // The inner may panic and we are OK with that, we just want to be able to run
                    // next time again.
                    .unwrap_or_else(PoisonError::into_inner);

                if let Some(handle) = locked.as_ref() {
                    trace!("Wrapping hooks into tokio handle");
                    handle.enter(inner);
                    trace!("Leaving tokio handle");
                } else {
                    // During startup, the handle/runtime is not *yet* ready. This is OK and
                    // expected, but we must run without it. And we also need to unlock that mutex,
                    // because the inner might actually contain the above init and want to provide
                    // the handle, so we don't want a deadlock.
                    drop(locked);
                    trace!("Running hooks without tokio handle, not available yet");
                    inner();
                }
            });

        ext
            .config_validator(init)
            .around_hooks(around_hooks)
            .run_around(move |spirit, inner| -> Result<(), AnyError> {
                // No need to worry about poisons here, we are going to be called just once
                let runtime = runtime.lock().unwrap().take().expect("Run even before config");
                debug!("Running with tokio handle");
                let result = runtime.enter(inner);
                if result.is_err() {
                    warn!("Tokio runtime initialization body returned an error, trying to shut everything down gracefully");
                    runtime.shutdown_background();
                    spirit.terminate();
                }
                result
                // Here the runtime is dropped. That makes it finish all the tasks.
            })
    }
}

/*
/// A body run on tokio runtime.
///
/// When specifying custom tokio runtime through the [`Runtime`](enum.Runtime.html) extension, this
/// is the future to be run inside the runtime.
pub type TokioBody = Box<dyn Future<Item = (), Error = AnyError> + Send>;

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
/// building is done), you need to install this manually *before* doing [`run`].
///
/// Note that the provided closures are `FnMut` mostly because `Box<FnOnce>` doesn't work. They
/// will be called just once, so you can use `Option<T>` inside and consume the value by
/// `take.unwrap()`.
///
/// # Runtime configuration
///
/// You may have noticed the callbacks here don't have access to configuration. If you intend to
/// configure eg. the number of threads from user configuration, use the [`ThreadPoolConfig`]
/// instead.
///
/// # Future compatibility
///
/// More variants may be added into the enum at any time. Such change will not be considered a
/// breaking change.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
///
/// use serde::Deserialize;
/// use spirit::{AnyError, Empty, Pipeline, Spirit};
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
/// fn connection() -> impl Future<Item = (), Error = AnyError> {
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
/// [`with_singleton`]: spirit::extension::Extensible::with_singleton
#[non_exhaustive]
pub enum Runtime {
    /// Use the threadpool runtime.
    ///
    /// The threadpool runtime is the default (both in tokio and spirit).
    ///
    /// This allows you to modify the builder prior to starting it, specifying custom options like
    /// number of threads.
    ThreadPool(Box<dyn FnMut(&mut runtime::Builder) + Send>),

    /// Use the current thread runtime.
    ///
    /// If you prefer to run everything in a single thread, use this variant. The provided closure
    /// can modify the builder prior to starting it.
    CurrentThread(Box<dyn FnMut(&mut runtime::current_thread::Builder) + Send>),

    /// Use completely custom runtime.
    ///
    /// The provided closure should start the runtime and execute the provided future on it,
    /// blocking until the runtime becomes empty.
    ///
    /// This allows combining arbitrary runtimes that are not directly supported by either tokio or
    /// spirit.
    Custom(Box<dyn FnMut(TokioBody) -> Result<(), AnyError> + Send>),
    // TODO: Support loading this from configuration? But it won't be possible to modify at
    // runtime, will it?
}

impl Default for Runtime {
    fn default() -> Self {
        Runtime::ThreadPool(Box::new(|_| {}))
    }
}

type InnerBody = Box<dyn FnOnce() -> Result<(), AnyError> + Send>;

impl Runtime {
    fn execute<O, C>(self, spirit: &Arc<Spirit<O, C>>, inner: InnerBody) -> Result<(), AnyError>
    where
        C: DeserializeOwned + Send + Sync + 'static,
        O: StructOpt + Send + Sync + 'static,
    {
        let spirit = Arc::clone(spirit);
        let fut = future::lazy(move || {
            inner().map_err(move |e| {
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
                runtime.run().map_err(AnyError::from)
            }
            Runtime::Custom(mut callback) => callback(Box::new(fut)),
        }
    }
}

impl<E> Extension<E> for Runtime
where
    E: Extensible<Ok = E>,
    E::Config: DeserializeOwned + Send + Sync + 'static,
    E::Opts: StructOpt + Send + Sync + 'static,
{
    fn apply(self, ext: E) -> Result<E, AnyError> {
        trace!("Wrapping in tokio runtime");
        ext.run_around(|spirit, inner| self.execute(spirit, inner))
    }
}

/// A configuration extension for the Tokio Threadpool runtime.
///
/// Using the [`extension`][ThreadPoolConfig::extension] or the
/// [`postprocess_extension`][ThreadPoolConfig::postprocess_extension] provides the [`Runtime`] to
/// the spirit application. However, this allows reading the parameters of the threadpool (mostly
/// number of threads) from the configuration instead of hardcoding it into the application.
///
/// # Panics
///
/// If this is inserted after something already registered a [`Runtime`].
///
/// # Examples
///
/// ```rust
/// use serde::Deserialize;
/// use spirit::{Empty, Spirit};
/// use spirit::prelude::*;
/// use spirit_tokio::runtime::ThreadPoolConfig;
///
/// #[derive(Debug, Default, Deserialize)]
/// struct Cfg {
///     #[serde(default)] // Allow empty configuration with default runtime
///     threadpool: ThreadPoolConfig,
/// }
///
/// impl Cfg {
///     fn threadpool(&self) -> ThreadPoolConfig {
///         self.threadpool.clone()
///     }
/// }
///
/// fn main() {
///     Spirit::<Empty, Cfg>::new()
///         .with(ThreadPoolConfig::extension(Cfg::threadpool))
///         .run(|_| {
///             // This runs inside a configured runtime
///             Ok(())
///         });
/// }
/// ```
#[derive(
    Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, StructDoc, Ord, PartialOrd, Hash,
)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub struct ThreadPoolConfig {
    /// Maximum number of asynchronous worker threads.
    ///
    /// These do most of the work. There's little reason to set it to more than number of CPUs, but
    /// it may make sense to set it lower.
    ///
    /// If not set, the application will start with number of CPUs available in the system.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub async_threads: Option<usize>,

    /// Maximum number of blocking worker threads.
    ///
    /// These do tasks that take longer time. This includes file IO and CPU intensive tasks.
    ///
    /// If not set, defaults to 100.
    ///
    /// Often, the application doesn't start these threads as they might not always be needed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocking_threads: Option<usize>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "spirit::utils::serialize_opt_duration",
        deserialize_with = "spirit::utils::deserialize_opt_duration",
        default
    )]

    /// How long to keep an idle thread around.
    ///
    /// A thread will be shut down if it sits around idle for this long. The default (unset) is
    /// never to shut it down.
    ///
    /// Accepts human-parsable times, like „3days“ or „5s“.
    pub keep_alive: Option<Duration>,
}

impl ThreadPoolConfig {
    /// The extension to be plugged in with [`with`].
    ///
    /// See the [example](#examples).
    ///
    /// [`with`]: spirit::extension::Extensible::with
    pub fn extension<O, C, F>(extract: F) -> impl Extension<Builder<O, C>>
    where
        F: Fn(&C) -> Self + Clone + Send + Sync + 'static,
        O: Debug + StructOpt + Send + Sync + 'static,
        C: DeserializeOwned + Send + Sync + 'static,
    {
        Self::postprocess_extension(extract, |_: &mut _| ())
    }

    /// Similar to [`extension`][ThreadPoolConfig::extension], but allows further tweaking.
    ///
    /// This allows to tweak the [threadpool builder][runtime::Builder] after it was pre-configured
    /// by the configuration file. This might be desirable, for example, if the application also
    /// wants to install an [`after_start`][runtime::Builder::after_start] or set the stack size
    /// which either can't or don't make sense to configure by the user.
    pub fn postprocess_extension<O, C, F, P>(extract: F, post: P) -> impl Extension<Builder<O, C>>
    where
        F: Fn(&C) -> Self + Clone + Send + Sync + 'static,
        P: FnOnce(&mut runtime::Builder) + Send + 'static,
        O: Debug + StructOpt + Send + Sync + 'static,
        C: DeserializeOwned + Send + Sync + 'static,
    {
        let mut post = Some(post);
        |mut builder: Builder<O, C>| {
            assert!(
                builder.singleton::<Runtime>(),
                "Tokio Runtime already inserted"
            );
            trace!("Inserting configurable tokio runtime");
            builder
                .on_config({
                    let extract = extract.clone();
                    let mut first = None;
                    move |_: &O, cfg: &Arc<C>| {
                        let cfg = extract(cfg);
                        if first.is_none() {
                            first = Some(cfg);
                        } else if first.as_ref() != Some(&cfg) {
                            warn!("Tokio threadpool configuration can't be changed at runtime");
                        }
                    }
                })
                .run_around(|spirit, inner| {
                    Runtime::ThreadPool({
                        let spirit = Arc::clone(spirit);
                        Box::new(move |builder| {
                            let cfg = extract(&spirit.config());
                            if let Some(threads) = cfg.async_threads {
                                builder.core_threads(threads);
                            }
                            if let Some(threads) = cfg.blocking_threads {
                                builder.blocking_threads(threads);
                            }
                            if let Some(alive) = cfg.keep_alive {
                                builder.keep_alive(Some(alive));
                            }
                            (post.take().unwrap())(builder)
                        })
                    })
                    .execute(spirit, inner)
                })
        }
    }
}
*/
