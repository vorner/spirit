use std::any::TypeId;
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use arc_swap::{ArcSwap, Lease};
use failure::{Error, Fail, ResultExt};
use log::{debug, info, trace};
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use signal_hook::iterator::Signals;
use structopt::StructOpt;

use crate::app::App;
use crate::bodies::{InnerBody, SpiritBody, WrapBody, Wrapper};
use crate::cfg_loader::{Builder as CfgBuilder, ConfigBuilder, Loader as CfgLoader};
use crate::empty::Empty;
use crate::extension::{Extensible, Extension};
use crate::utils;
use crate::validation::Action;

#[derive(Debug, Fail)]
#[fail(display = "Config validation failed with {} errors", _0)]
pub struct ValidationError(usize);

struct Hooks<O, C> {
    config: Vec<Box<FnMut(&O, &Arc<C>) + Send>>,
    config_loader: CfgLoader,
    config_mutators: Vec<Box<FnMut(&mut C) + Send>>,
    config_validators: Vec<Box<FnMut(&Arc<C>, &Arc<C>, &O) -> Result<Action, Error> + Send>>,
    sigs: HashMap<libc::c_int, Vec<Box<FnMut() + Send>>>,
    singletons: HashSet<TypeId>,
    terminate: Vec<Box<FnMut() + Send>>,
}

impl<O, C> Default for Hooks<O, C> {
    fn default() -> Self {
        Hooks {
            config: Vec::new(),
            config_loader: CfgBuilder::new().build_no_opts(),
            config_mutators: Vec::new(),
            config_validators: Vec::new(),
            sigs: HashMap::new(),
            singletons: HashSet::new(),
            terminate: Vec::new(),
        }
    }
}

/// The main manipulation handle/struct of the library.
///
/// This gives access to the runtime control over the behaviour of the spirit library and allows
/// accessing current configuration and manipulate the behaviour to some extent.
///
/// Note that the functionality of the library is not disturbed by dropping this, you simply lose
/// the ability to control the library.
///
/// By creating this (with the build pattern), you start a background thread that keeps track of
/// signals, reloading configuration and other bookkeeping work.
///
/// The passed callbacks are run in the service threads if they are caused by the signals. They,
/// however, can be run in any other thread when the controlled actions are invoked manually.
///
/// This is supposed to be a singleton (it is not enforced, but having more of them around is
/// probably not what you want).
///
/// # Warning
///
/// Only one callback is allowed to run at any given time. This makes it easier to write the
/// callbacks (eg. transitioning between configurations at runtime), but if you ever invoke a
/// method that contains callbacks from within a callback, you'll get a deadlock.
///
/// # Examples
///
/// ```rust
/// use spirit::prelude::*;
///
/// Spirit::<Empty, Empty>::new()
///     .on_config(|_opts, _new_cfg| {
///         // Adapt to new config here
///     })
///     .run(|_spirit| {
///         // Application runs here
///         Ok(())
///     });
/// ```
pub struct Spirit<O = Empty, C = Empty> {
    config: ArcSwap<C>,
    hooks: Mutex<Hooks<O, C>>,
    // TODO: Mode selection for directories
    opts: O,
    terminate: AtomicBool,
    signals: Signals,
}

impl<O, C> Spirit<O, C>
where
    C: Default + DeserializeOwned + Send + Sync,
    O: StructOpt,
{
    /// A constructor with default initial config.
    ///
    /// Before the application successfully loads the first config, there still needs to be
    /// something (passed, for example, to validation callbacks) This puts the default value in
    /// there.
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> Builder<O, C> {
        Spirit::with_initial_config(C::default())
    }
}

impl<O, C> Spirit<O, C>
where
    C: DeserializeOwned + Send + Sync,
    O: StructOpt,
{
    /// Similar to [`new`](#method.new), but with specific initial config value
    pub fn with_initial_config(config: C) -> Builder<O, C> {
        Builder {
            before_bodies: Vec::new(),
            before_config: Vec::new(),
            body_wrappers: Vec::new(),
            config,
            config_loader: CfgBuilder::new(),
            config_hooks: Vec::new(),
            config_mutators: Vec::new(),
            config_validators: Vec::new(),
            opts: PhantomData,
            sig_hooks: HashMap::new(),
            singletons: HashSet::new(),
            terminate_hooks: Vec::new(),
        }
    }
    /// Access the parsed command line.
    ///
    /// This gives the access to the type declared when creating `Spirit`. The content doesn't
    /// change (the command line is parsed just once) and it does not contain the options added by
    /// Spirit itself.
    pub fn cmd_opts(&self) -> &O {
        &self.opts
    }

    /// Access to the current configuration.
    ///
    /// This returns the *current* version of configuration. Note that you can keep hold of this
    /// snapshot of configuration (which does not change), but calling this again might give a
    /// different config.
    ///
    /// If you *do* want to hold onto the returned configuration for longer time, upgrade the
    /// returned `Lease` to `Arc`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use arc_swap::Lease;
    /// use spirit::prelude::*;
    ///
    /// let app = Spirit::<Empty, Empty>::new()
    ///     .build(false)
    ///     .unwrap();
    ///
    /// let spirit = app.spirit();
    ///
    /// let old_config = Lease::upgrade(&spirit.config());
    /// # drop(old_config);
    /// ```
    ///
    /// # Notes
    ///
    /// If created with the [`with_config_storage`](#method.with_config_storage), the current
    /// configuration is also available through that storage. This allows, for example, having the
    /// configuration in a global variable, for example.
    pub fn config(&self) -> Lease<Arc<C>> {
        self.config.lease()
    }

    /// Force reload of configuration.
    ///
    /// The configuration gets reloaded either when the process receives `SIGHUP` or when this
    /// method is called manually.
    ///
    /// This is what happens:
    /// * The configuration is loaded from all places.
    /// * Configuration mutators are run and can modify the loaded configuration.
    /// * Validation callbacks are called (all of them).
    /// * If no validation callback returns an error, success callbacks of the validation results
    ///   are called. Otherwise, abort callbacks are called.
    /// * Logging is reopened in the new form.
    /// * The configuration is published into the storage.
    /// * The `on_config` callbacks are called.
    ///
    /// If any step fails, it is aborted and the old configuration is preserved.
    ///
    /// # Warning
    ///
    /// The Spirit allows to run only one callback at a time (even from multiple threads), to make
    /// reasoning about configuration transitions and such easier (and to make sure the callbacks
    /// don't have to by `Sync`). That, however, means that you can't call `config_reload` or
    /// [`terminate`](#method.terminate) from any callback (that would lead to a deadlock).
    pub fn config_reload(&self) -> Result<(), Error> {
        let mut new = self.load_config().context("Failed to load configuration")?;
        // The lock here is across the whole processing, to avoid potential races in logic
        // processing. This makes writing the hooks correctly easier.
        let mut hooks = self.hooks.lock();
        debug!("Running {} config mutators", hooks.config_mutators.len());
        for m in &mut hooks.config_mutators {
            m(&mut new);
        }
        let new = Arc::new(new);
        let old = self.config.load();
        debug!("Running {} config validators", hooks.config_validators.len());
        let mut errors = 0;
        let mut actions = Vec::with_capacity(hooks.config_validators.len());
        for v in hooks.config_validators.iter_mut() {
            match v(&old, &new, &self.opts) {
                Ok(ac) => actions.push(ac),
                Err(e) => {
                    errors += 1;
                    utils::log_error(module_path!(), &e);
                }
            }
        }

        if errors == 0 {
            debug!("Validation successful, switching to new config");
            for a in actions {
                a.run(true);
            }
        } else {
            debug!("Rolling back validation attempt");
            for a in actions {
                a.run(false);
            }
            return Err(ValidationError(errors).into());
        }

        // Once everything is validated, switch to the new config
        self.config.store(Arc::clone(&new));
        debug!("Running {} post-configuration hooks", hooks.config.len());
        for hook in &mut hooks.config {
            hook(&self.opts, &new);
        }
        debug!("Configuration reloaded");
        Ok(())
    }

    /// Is the application in the shutdown phase?
    ///
    /// This can be used if the daemon does some kind of periodic work, every loop it can check if
    /// the application should shut down.
    ///
    /// The other option is to hook into [`on_terminate`](struct.Builder.html#method.on_terminate)
    /// and shut things down (like drop some futures and make the tokio event loop empty).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::thread;
    /// use std::time::Duration;
    ///
    /// use spirit::prelude::*;
    ///
    /// let app = Spirit::<Empty, Empty>::new()
    ///     .build(false)
    ///     .unwrap();
    ///
    /// let spirit = app.spirit();
    ///
    /// while !spirit.is_terminated() {
    ///     thread::sleep(Duration::from_millis(100));
    /// #   spirit.terminate();
    /// }
    /// ```
    pub fn is_terminated(&self) -> bool {
        self.terminate.load(Ordering::Relaxed)
    }

    /// Terminate the application in a graceful manner.
    ///
    /// The Spirit/application can be terminated either by one of termination signals (`SIGTERM`,
    /// `SIGQUIT`, `SIGINT`) or by manually calling this method.
    ///
    /// The termination does this:
    ///
    /// * Calls the `on_terminate` callbacks.
    /// * Sets the [`is_terminated`](#method.is_terminated) flag is set.
    /// * Drops all callbacks from spirit. This allows destruction/termination of parts of program
    ///   by dropping remote handles or similar things.
    /// * The background thread terminates.
    ///
    /// # Warning
    ///
    /// The Spirit guarantees only one callback runs at a time. That means you can't call this from
    /// within a callback (it would lead to deadlock).
    pub fn terminate(&self) {
        debug!("Running termination hooks");
        for hook in &mut self.hooks.lock().terminate {
            hook();
        }
        self.terminate.store(true, Ordering::Relaxed);
        *self.hooks.lock() = Hooks::default();
    }

    fn background(&self, signals: &Signals) {
        debug!("Starting background processing");
        for signal in signals.forever() {
            debug!("Received signal {}", signal);
            let term = match signal {
                libc::SIGHUP => {
                    let _ = utils::log_errors(|| self.config_reload());
                    false
                }
                libc::SIGTERM | libc::SIGINT | libc::SIGQUIT => {
                    self.terminate();
                    true
                }
                // Some other signal, only for the hook benefit
                _ => false,
            };

            if let Some(hooks) = self.hooks.lock().sigs.get_mut(&signal) {
                for hook in hooks {
                    hook();
                }
            }

            if term {
                debug!("Terminating the background thread");
                return;
            }
        }
        unreachable!("Signals run forever");
    }

    fn load_config(&self) -> Result<C, Error> {
        self.hooks.lock().config_loader.load()
    }
}

impl<O, C> Extensible for &Arc<Spirit<O, C>>
where
    C: DeserializeOwned + Send + Sync,
    O: StructOpt,
{
    type Opts = O;
    type Config = C;
    type Ok = Self;
    const STARTED: bool = true;

    fn before_config<F>(self, cback: F) -> Result<Self, Error>
    where
        F: FnOnce(&Self::Config, &Self::Opts) -> Result<(), Error> + Send + 'static
    {
        trace!("Running just added before_config");
        cback(&self.config(), self.cmd_opts())?;
        Ok(self)
    }

    fn config_validator<F>(self, mut f: F) -> Result<Self, Error>
    where
        F: FnMut(&Arc<C>, &Arc<C>, &O) -> Result<Action, Error> + Send + 'static,
    {
        trace!("Adding config validator at runtime");
        let mut hooks = self.hooks.lock();
        let cfg = Lease::into_upgrade(self.config());
        f(&cfg, &cfg, self.cmd_opts())?.run(true);
        hooks.config_validators.push(Box::new(f));
        Ok(self)
    }

    fn config_mutator<F>(self, f: F) -> Self
    where
        F: FnMut(&mut C) + Send + 'static,
    {
        trace!("Adding config mutator at runtime");
        let mut hooks = self.hooks.lock();
        hooks.config_mutators.push(Box::new(f));
        self
    }

    fn on_config<F: FnMut(&O, &Arc<C>) + Send + 'static>(self, mut hook: F) -> Self {
        trace!("Adding config hook at runtime");
        let mut hooks = self.hooks.lock();
        hook(self.cmd_opts(), &Lease::upgrade(&self.config()));
        hooks.config.push(Box::new(hook));
        self
    }

    fn on_signal<F>(self, signal: libc::c_int, hook: F) -> Result<Self, Error>
    where
        F: FnMut() + Send + 'static,
    {
        trace!("Adding signal hook at runtime");
        self.signals.add_signal(signal)?;
        self.hooks
            .lock()
            .sigs
            .entry(signal)
            .or_insert_with(Vec::new)
            .push(Box::new(hook));
        Ok(self)
    }

    fn on_terminate<F: FnMut() + Send + 'static>(self, hook: F) -> Self {
        trace!("Running termination hook at runtime");
        let mut hooks = self.hooks.lock();
        hooks.terminate.push(Box::new(hook));
        // FIXME: Is there possibly a race condition because the lock and the is_terminated are
        // unsychronized/not SeqCst?
        if self.is_terminated() {
            (hooks.terminate.last_mut().unwrap())();
        }
        self
    }

    fn with<E>(self, ext: E) -> Result<Self::Ok, Error>
    where
        E: Extension<Self>,
    {
        ext.apply(self)
    }

    fn singleton<T: 'static>(&mut self) -> bool {
        self.hooks.lock().singletons.insert(TypeId::of::<T>())
    }

    fn run_before<B>(self, body: B) -> Result<Self, Error>
    where
        B: FnOnce(&Arc<Spirit<Self::Opts, Self::Config>>) -> Result<(), Error> + Send + 'static,
    {
        body(self).map(|()| self)
    }

    fn run_around<W>(self, _wrapper: W) -> Result<Self, Error>
    where
        W: FnOnce(&Arc<Spirit<Self::Opts, Self::Config>>, InnerBody) -> Result<(), Error>
            + Send
            + 'static,
    {
        // XXX: Link to some documentation/help how to resolve
        panic!("Wrapping body while already running is not possible, move this to the builder");
    }

    fn with_singleton<T>(mut self, singleton: T) -> Result<Self, Error>
    where
        T: Extension<Self::Ok> + 'static,
    {
        if self.singleton::<T>() {
            self.with(singleton)
        } else {
            Ok(self)
        }
    }
}

// TODO: Implement for non-ref Arc
//
// TODO: Document some of the quirks of specific implementations.

/// The builder of [`Spirit`](struct.Spirit.html).
///
/// This is returned by the [`Spirit::new`](struct.Spirit.html#new).
#[must_use = "The builder is inactive without calling `run` or `build`"]
pub struct Builder<O = Empty, C = Empty> {
    before_bodies: Vec<SpiritBody<O, C>>,
    before_config: Vec<Box<FnMut(&C, &O) -> Result<(), Error> + Send>>,
    body_wrappers: Vec<Wrapper<O, C>>,
    config: C,
    config_loader: CfgBuilder,
    config_hooks: Vec<Box<FnMut(&O, &Arc<C>) + Send>>,
    config_mutators: Vec<Box<FnMut(&mut C) + Send>>,
    config_validators: Vec<Box<FnMut(&Arc<C>, &Arc<C>, &O) -> Result<Action, Error> + Send>>,
    opts: PhantomData<O>,
    sig_hooks: HashMap<libc::c_int, Vec<Box<FnMut() + Send>>>,
    singletons: HashSet<TypeId>,
    terminate_hooks: Vec<Box<FnMut() + Send>>,
}

impl<O, C> Builder<O, C>
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: StructOpt + Sync + Send + 'static,
{

    /// Sets the inner config loader.
    ///
    /// Instead of configuring the configuration loading on the spirit [`Builder`], the inner
    /// config loader [`Builder`][crate::cfg_loader::Builder] can be set directly.
    pub fn config_loader(self, loader: CfgBuilder) -> Self {
        Self {
            config_loader: loader,
            ..self
        }
    }
}

impl<O, C> ConfigBuilder for Builder<O, C> {
    /// Sets the configuration paths in case the user doesn't provide any.
    ///
    /// This replaces any previously set default paths. If none are specified and the user doesn't
    /// specify any either, no config is loaded (but it is not an error, simply the defaults will
    /// be used, if available).
    fn config_default_paths<P, I>(self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        Self {
            config_loader: self.config_loader.config_default_paths(paths),
            ..self
        }
    }

    /// Specifies the default configuration.
    ///
    /// This „loads“ the lowest layer of the configuration from the passed string. The expected
    /// format is TOML.
    fn config_defaults<D: Into<String>>(self, config: D) -> Self {
        Self {
            config_loader: self.config_loader.config_defaults(config),
            ..self
        }
    }

    /// Enables loading configuration from environment variables.
    ///
    /// If this is used, after loading the normal configuration files, the environment of the
    /// process is examined. Variables with the provided prefix are merged into the configuration.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use failure::Error;
    /// use serde::Deserialize;
    /// use spirit::prelude::*;
    ///
    /// #[derive(Default, Deserialize)]
    /// struct Cfg {
    ///     message: String,
    /// }
    ///
    /// const DEFAULT_CFG: &str = r#"
    /// message = "Hello"
    /// "#;
    ///
    /// fn main() {
    ///     Spirit::<Empty, Cfg>::new()
    ///         .config_defaults(DEFAULT_CFG)
    ///         .config_env("HELLO")
    ///         .run(|spirit| -> Result<(), Error> {
    ///             println!("{}", spirit.config().message);
    ///             Ok(())
    ///         });
    /// }
    /// ```
    ///
    /// If run like this, it'll print `Hi`. The environment takes precedence ‒ even if there was
    /// configuration file and it set the `message`, the `Hi` here would win.
    ///
    /// ```sh
    /// HELLO_MESSAGE="Hi" ./hello
    /// ```
    fn config_env<E: Into<String>>(self, env: E) -> Self {
        Self {
            config_loader: self.config_loader.config_env(env),
            ..self
        }
    }

    /// Sets a configuration dir filter.
    ///
    /// If the user passes a directory path instead of a file path, the directory is traversed
    /// (every time the configuration is reloaded, so if files are added or removed, it is
    /// reflected) and files passing this filter are merged into the configuration, in the
    /// lexicographical order of their file names.
    ///
    /// There's ever only one filter and the default one passes no files (therefore, directories
    /// are ignored by default).
    ///
    /// The filter has no effect on files, only on loading directories. Only files directly in the
    /// directory are loaded ‒ subdirectories are not traversed.
    ///
    /// For more convenient ways to set the filter, see [`config_ext`](#method.config_ext) and
    /// [`config_exts`](#method.config_exts).
    fn config_filter<F: FnMut(&Path) -> bool + Send + 'static>(self, filter: F) -> Self {
        Self {
            config_loader: self.config_loader.config_filter(filter),
            ..self
        }
    }
}

impl<O, C> Extensible for Builder<O, C> {
    type Opts = O;
    type Config = C;
    type Ok = Self;
    const STARTED: bool = false;

    fn before_config<F>(mut self, cback: F) -> Result<Self, Error>
    where
        F: FnOnce(&C, &O) -> Result<(), Error> + Send + 'static,
    {
        let mut cback = Some(cback);
        let cback = move |conf: &C, opts: &O| (cback.take().unwrap())(conf, opts);
        self.before_config.push(Box::new(cback));
        Ok(self)
    }

    fn config_validator<F>(self, f: F) -> Result<Self, Error>
    where
        F: FnMut(&Arc<C>, &Arc<C>, &O) -> Result<Action, Error> + Send + 'static,
    {
        let mut validators = self.config_validators;
        validators.push(Box::new(f));
        Ok(Self {
            config_validators: validators,
            ..self
        })
    }

    fn config_mutator<F>(self, f: F) -> Self
    where
        F: FnMut(&mut C) + Send + 'static,
    {
        let mut mutators = self.config_mutators;
        mutators.push(Box::new(f));
        Self {
            config_mutators: mutators,
            ..self
        }
    }

    fn on_config<F: FnMut(&O, &Arc<C>) + Send + 'static>(self, hook: F) -> Self {
        let mut hooks = self.config_hooks;
        hooks.push(Box::new(hook));
        Self {
            config_hooks: hooks,
            ..self
        }
    }

    fn on_signal<F>(self, signal: libc::c_int, hook: F) -> Result<Self, Error>
    where
        F: FnMut() + Send + 'static,
    {
        let mut hooks = self.sig_hooks;
        hooks
            .entry(signal)
            .or_insert_with(Vec::new)
            .push(Box::new(hook));
        Ok(Self {
            sig_hooks: hooks,
            ..self
        })
    }

    fn on_terminate<F: FnMut() + Send + 'static>(self, hook: F) -> Self {
        let mut hooks = self.terminate_hooks;
        hooks.push(Box::new(hook));
        Self {
            terminate_hooks: hooks,
            ..self
        }
    }

    fn with<E>(self, ext: E) -> Result<Self::Ok, Error>
    where
        E: Extension<Self>,
    {
        ext.apply(self)
    }

    fn singleton<T: 'static>(&mut self) -> bool {
        self.singletons.insert(TypeId::of::<T>())
    }

    fn run_before<B>(mut self, body: B) -> Result<Self, Error>
    where
        B: FnOnce(&Arc<Spirit<O, C>>) -> Result<(), Error> + Send + 'static,
    {
        self.before_bodies.push(Box::new(Some(body)));
        Ok(self)
    }

    fn run_around<W>(mut self, wrapper: W) -> Result<Self, Error>
    where
        W: FnOnce(&Arc<Spirit<O, C>>, InnerBody) -> Result<(), Error> + Send + 'static,
    {
        let wrapper = move |(spirit, inner): (&_, _)| wrapper(spirit, inner);
        self.body_wrappers.push(Box::new(Some(wrapper)));
        Ok(self)
    }

    fn with_singleton<T>(mut self, singleton: T) -> Result<Self, Error>
    where
        T: Extension<Self::Ok> + 'static,
    {
        if self.singleton::<T>() {
            self.with(singleton)
        } else {
            Ok(self)
        }
    }
}

pub trait SpiritBuilder: ConfigBuilder + Extensible {
    /// Finish building the Spirit.
    ///
    /// This transitions from the configuration phase of Spirit to actually creating it. This loads
    /// the configuration and executes it for the first time. It launches the background thread for
    /// listening to signals and reloading configuration if the `background_thread` parameter is
    /// set to true.
    ///
    /// This starts listening for signals, loads the configuration for the first time and starts
    /// the background thread.
    ///
    /// This version returns the spirit (or error) and error handling is up to the caller. If you
    /// want spirit to take care of nice error logging (even for your application's top level
    /// errors), use [`run`](#method.run).
    ///
    /// # Result
    ///
    /// On success, this returns three things:
    ///
    /// * The `spirit` handle, allowing to manipulate it (shutdown, read configuration, ...)
    /// * The before-body hooks (see [`before_body`](#method.before_body).
    /// * The body wrappers ([`body_wrapper`](#method.body_wrapper)).
    ///
    /// The two latter ones are often set by helpers, so you should not ignore them.
    ///
    /// # Warning
    ///
    /// If asked to go to background (when you're using the
    /// [`spirit-daemonize`](https://crates.io/crates/spirit-daemonize) crate, this uses `fork`.
    /// Therefore, start any threads after you call `build` (or from within [`run`](#method.run)),
    /// or you'll lose them ‒ only the thread doing fork is preserved across it.
    // TODO: The new return value
    fn build(self, background_thread: bool) -> Result<App<Self::Opts, Self::Config>, Error>;

    /// Build the spirit and run the application, handling all relevant errors.
    ///
    /// In case an error happens (either when creating the Spirit, or returned by the callback),
    /// the errors are logged (either to the place where logs are sent to in configuration, or to
    /// stderr if the error happens before logging is initialized ‒ for example if configuration
    /// can't be read). The application then terminates with failure exit code.
    ///
    /// It first wraps all the calls in the provided wrappers
    /// ([`body_wrapper`](#method.body_wrapper)) and runs the before body hooks
    /// ([`before_body`](#method.before_body)) before starting the real body provided as parameter.
    /// These are usually provided by helpers.
    ///
    /// ```rust
    /// use std::thread;
    /// use std::time::Duration;
    ///
    /// use spirit::prelude::*;
    ///
    /// Spirit::<Empty, Empty>::new()
    ///     .run(|spirit| {
    ///         while !spirit.is_terminated() {
    ///             // Some reasonable work here
    ///             thread::sleep(Duration::from_millis(100));
    /// #           spirit.terminate();
    ///         }
    ///
    ///         Ok(())
    ///     });
    /// ```
    fn run<B>(self, body: B)
    where
        B: FnOnce(&Arc<Spirit<Self::Opts, Self::Config>>) -> Result<(), Error> + Send + 'static;
}

impl<O, C> SpiritBuilder for Builder<O, C>
where
    Self::Config: DeserializeOwned + Send + Sync + 'static,
    Self::Opts: StructOpt + Sync + Send + 'static,
{
    fn build(mut self, background_thread: bool) -> Result<App<O, C>, Error> {
        debug!("Building the spirit");
        let (opts, loader) = self.config_loader.build::<Self::Opts>();
        for before_config in &mut self.before_config {
            before_config(&self.config, &opts).context("The before-config phase failed")?;
        }
        let interesting_signals = self
            .sig_hooks
            .keys()
            .chain(&[libc::SIGHUP, libc::SIGTERM, libc::SIGQUIT, libc::SIGINT])
            .cloned()
            .collect::<HashSet<_>>(); // Eliminate duplicates
        let config = ArcSwap::from(Arc::from(self.config));
        let signals = Signals::new(interesting_signals)?;
        let signals_spirit = signals.clone();
        let spirit = Spirit {
            config,
            hooks: Mutex::new(Hooks {
                config: self.config_hooks,
                config_loader: loader,
                config_mutators: self.config_mutators,
                config_validators: self.config_validators,
                sigs: self.sig_hooks,
                singletons: self.singletons,
                terminate: self.terminate_hooks,
            }),
            opts,
            terminate: AtomicBool::new(false),
            signals: signals_spirit,
        };
        spirit
            .config_reload()
            .context("Problem loading the initial configuration")?;
        let spirit = Arc::new(spirit);
        if background_thread {
            let spirit_bg = Arc::clone(&spirit);
            thread::Builder::new()
                .name("spirit".to_owned())
                .spawn(move || {
                    loop {
                        // Note: we run a bunch of callbacks inside the service thread. We restart
                        // the thread if it fails.
                        let run = AssertUnwindSafe(|| spirit_bg.background(&signals));
                        if panic::catch_unwind(run).is_err() {
                            // FIXME: Something better than this to prevent looping?
                            thread::sleep(Duration::from_secs(1));
                            info!("Restarting the spirit service thread after a panic");
                        } else {
                            // Willingly terminated
                            break;
                        }
                    }
                })
                .unwrap(); // Could fail only if the name contained \0
        }
        debug!(
            "Building bodies from {} before-bodies and {} wrappers",
            self.before_bodies.len(),
            self.body_wrappers.len()
        );
        let spirit_body = Arc::clone(&spirit);
        let bodies = self.before_bodies;
        let inner = move |()| {
            for mut body in bodies {
                body.run(&spirit_body)?;
            }
            Ok(())
        };
        let body_wrappers = self.body_wrappers;
        let inner = InnerBody(Box::new(Some(inner)));
        let spirit_body = Arc::clone(&spirit);
        let mut wrapped = WrapBody(Box::new(Some(|inner: InnerBody| inner.run())));
        for mut wrapper in body_wrappers.into_iter().rev() {
            // TODO: Can we get rid of this clone?
            let spirit = Arc::clone(&spirit_body);
            let applied = move |inner: InnerBody| wrapper.run((&spirit, inner));
            wrapped = WrapBody(Box::new(Some(applied)));
        }
        Ok(App::new(spirit, inner, wrapped))
    }

    fn run<B: FnOnce(&Arc<Spirit<O, C>>) -> Result<(), Error> + Send + 'static>(self, body: B) {
        Ok(self).run(body);
    }
}

impl<O, C> SpiritBuilder for Result<Builder<O, C>, Error>
where
    Self::Config: DeserializeOwned + Send + Sync + 'static,
    Self::Opts: StructOpt + Sync + Send + 'static,
{
    fn build(self, background_thread: bool) -> Result<App<O, C>, Error> {
        self.and_then(|b| b.build(background_thread))
    }
    fn run<B: FnOnce(&Arc<Spirit<O, C>>) -> Result<(), Error> + Send + 'static>(self, body: B) {
        let result = utils::log_errors_named("top-level", || {
            let me = self?;
            let app = me.build(true)?;
            let spirit = Arc::clone(app.spirit());
            let body = move || body(&spirit);
            app.run(body)
        });
        if result.is_err() {
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: this is not run, we only test if it compiles
    fn _nonref_spirit_extensible() {
        let app = Spirit::<Empty, Empty>::new().build(false).unwrap();
        // We test the trait actually works even when we have owned value, not reference only.
        let spirit = Arc::clone(app.spirit());
        spirit
            .on_terminate(|| ())
            .on_config(|_opts, _cfg| ());
    }
}
