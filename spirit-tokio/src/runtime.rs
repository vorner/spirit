//! An extension to start the tokio runtime at the appropriate time.

use std::sync::{Arc, Mutex, PoisonError};

use err_context::AnyError;
use err_context::prelude::*;
use log::{debug, trace, warn};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use spirit::extension::{Extensible, Extension};
use spirit::validation::Action;
#[cfg(feature = "cfg-help")]
use structdoc::StructDoc;
use structopt::StructOpt;
use tokio::runtime::{Builder, Runtime};
use tokio::sync::oneshot;

// From tokio documentation
const DEFAULT_MAX_THREADS: usize = 512;
const THREAD_NAME: &str = "tokio-runtime-worker";

/// Configuration for building a threaded runtime.
#[cfg(feature = "rt-from-cfg")]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Hash, Serialize)]
#[serde(rename_all = "kebab-case", default)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[non_exhaustive]
pub struct Cfg {
    /// Number of threads used for asynchronous processing.
    ///
    /// Defaults to number of available CPUs in the system if left unconfigured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub core_threads: Option<usize>,

    /// Total maximum number of threads, including ones for blocking (non-async) operations.
    ///
    /// Must be at least `core-threads`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_threads: Option<usize>,

    /// The thread name to use for the worker threads.
    pub thread_name: String,
}

#[cfg(feature = "rt-from-cfg")]
impl Default for Cfg {
    fn default() -> Self {
        Cfg {
            core_threads: None,
            max_threads: None,
            thread_name: THREAD_NAME.to_owned(),
        }
    }
}

#[cfg(feature = "rt-from-cfg")]
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

/// A [`spirit`] [`Extension`] to inject a [`tokio`] runtime.
///
/// This, when inserted into spirit [`Builder`][spirit::Builder] with the
/// [`with_singleton`][Extensible::with_singleton] will provide the application with a tokio
/// runtime.
///
/// This will:
///
/// * Run the application body inside the runtime (to allow spawning tasks and creating tokio
///   resources).
/// * Run the configuration/termination/signal hooks inside the async context of the runtime (for
///   similar reasons).
/// * Keep the runtime running until [`terminate`][spirit::Spirit::terminate] is invoked (either
///   explicitly or by CTRL+C or similar).
///
/// A default instance ([`Tokio::Default`]) is inserted by pipelines containing the
/// [`FutureInstaller`][crate::FutureInstaller].
#[non_exhaustive]
#[allow(clippy::type_complexity)] // While complex, the types are probably more readable inline
pub enum Tokio<O, C> {
    /// Provides the equivalent of [`Runtime::new`].
    Default,

    /// Allows the caller to provide arbitrary constructor for the [`Runtime`].
    ///
    /// This variant also allows creating the basic (non-threaded) scheduler if needed. Note that
    /// some operations with such runtime are prone to cause deadlocks.
    Custom(Box<dyn FnMut(&O, &Arc<C>) -> Result<Runtime, AnyError> + Send>),

    /// Uses configuration for constructing the [`Runtime`].
    ///
    /// This'll use the extractor (the first closure) to get a [`Cfg`], create a [`Builder`] based
    /// on that. It'll explicitly enable all the drivers and enable threaded runtime. Then it calls
    /// the postprocessor (the second closure) to turn it into the [`Runtime`].
    ///
    /// This is the more general form. If you're fine with just basing it on the configuration
    /// without much tweaking, you can use [`Tokio::from_cfg`] (which will create this variant with
    /// reasonable value of preprocessor).
    ///
    /// This is available only with the [`rt-from-cfg`] feature enabled.
    #[cfg(feature = "rt-from-cfg")]
    FromCfg(
        Box<dyn FnMut(&O, &C) -> Cfg + Send>,
        Box<dyn FnMut(Builder) -> Result<Runtime, AnyError> + Send>,
    ),
}

impl<O, C> Tokio<O, C> {
    /// Simplified construction from configuration.
    ///
    /// This is similar to [`Tokio::FromCfg`]. However, the extractor takes only the configuration
    /// structure, not the command line options. Furthermore, post-processing is simply calling
    /// [`Builder::build`], without a chance to tweak.
    #[cfg(feature = "rt-from-cfg")]
    pub fn from_cfg<E>(mut extractor: E) -> Self
    where
        E: FnMut(&C) -> Cfg + Send + 'static,
    {
        let extractor = move |_opts: &O, cfg: &C| extractor(&cfg);
        let finish = |mut builder: Builder| -> Result<Runtime, AnyError> {
            Ok(builder.build()?)
        };
        Tokio::FromCfg(
            Box::new(extractor),
            Box::new(finish),
        )
    }

    /// Method to create the runtime.
    ///
    /// This can be used when not taking advantage of the spirit auto-management features.
    pub fn create(&mut self, opts: &O, cfg: &Arc<C>) -> Result<Runtime, AnyError> {
        match self {
            Tokio::Default => Runtime::new().map_err(AnyError::from),
            Tokio::Custom(ctor) => ctor(opts, cfg),
            #[cfg(feature = "rt-from-cfg")]
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
            #[cfg(feature = "rt-from-cfg")]
            let mut prev_cfg = None;

            // This runs as config validator to be sooner in the chain.
            move |_: &_, cfg: &Arc<_>, opts: &_| -> Result<Action, AnyError> {
                if initialized {
                    #[cfg(feature = "rt-from-cfg")]
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
                    #[cfg(feature = "rt-from-cfg")]
                    if let Tokio::FromCfg(extract, _) = &mut self {
                        prev_cfg = Some(extract(opts, &cfg));
                    }
                }
                Ok(Action::new())
            }
        };

        let (terminate_send, terminate_recv) = oneshot::channel();

        // Ugly hack: seems like Rust is not exactly ready to deal with our crazy type in here and
        // deduce it in just the function call :-(. Force it by an explicit type.
        #[allow(clippy::type_complexity)] // We are glad we managed to make it compile at all
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
            .on_terminate(|| {
                let _ = terminate_send.send(());
            })
            .config_validator(init)
            .around_hooks(around_hooks)
            .run_around(move |spirit, inner| -> Result<(), AnyError> {
                // No need to worry about poisons here, we are going to be called just once
                let mut runtime = runtime.lock().unwrap().take().expect("Run even before config");
                debug!("Running with tokio handle");
                let result = runtime.enter(inner);
                debug!("Inner bodies ended");
                if result.is_ok() {
                    debug!("Waiting for spirit to terminate");
                    let _ = runtime.block_on(terminate_recv);
                    debug!("Spirit signalled termination to runtime");
                    drop(runtime);
                } else {
                    warn!("Tokio runtime initialization body returned an error, trying to shut everything down gracefully");
                    runtime.shutdown_background();
                    spirit.terminate();
                }
                result
                // Here the runtime is dropped. That makes it finish all the tasks.
            })
    }
}
