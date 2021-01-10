use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::marker::PhantomData;
use std::mem;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use arc_swap::ArcSwap;
use err_context::prelude::*;
use libc::pid_t;
use log::{debug, error, info, trace};
use serde::de::DeserializeOwned;
use signal_hook::iterator::exfiltrator::origin::WithOrigin;
use signal_hook::iterator::{Handle as SigHandle, SignalsInfo};
use structopt::StructOpt;

use crate::app::App;
use crate::bodies::{HookWrapper, InnerBody, SpiritBody, WrapBody, Wrapper};
use crate::cfg_loader::{Builder as CfgBuilder, ConfigBuilder, Loader as CfgLoader};
use crate::empty::Empty;
use crate::error;
use crate::extension::{Autojoin, Extensible, Extension};
use crate::fragment::pipeline::MultiError;
use crate::validation::Action;
use crate::AnyError;

fn get_proc_name(pid: Option<pid_t>) -> String {
    fn inner(pid: pid_t) -> Result<String, AnyError> {
        let cmdline = std::fs::read(format!("/proc/{}/cmdline", pid))?;
        let binary = cmdline.split(|b| *b == 0).next().unwrap_or_default();
        Ok(String::from_utf8_lossy(binary).into_owned())
    }
    match pid.map(inner) {
        None | Some(Err(_)) => String::new(),
        Some(Ok(s)) if s.is_empty() => ", (Unnamed process)".to_owned(),
        Some(Ok(s)) => format!(", Process: {} (best effort guess)", s),
    }
}

type Signals = SignalsInfo<WithOrigin>;

/// A recursive implementation of the logic of wrapping of hooks.
///
/// The last one added is the innermost.
fn wrap_around_hooks<R, I>(wrappers: &mut [Box<dyn HookWrapper>], inner: I) -> R
where
    I: FnOnce() -> R,
{
    if let Some((outer, rest)) = wrappers.split_first_mut() {
        let mut output = None;
        let inner_wrapped = || {
            output = Some(wrap_around_hooks(rest, inner));
        };
        outer(Box::new(inner_wrapped));
        output.expect("Inner body not called by a wrapper")
    } else {
        inner()
    }
}

#[derive(Copy, Clone, Debug)]
pub struct ValidationError(usize, usize);

impl Display for ValidationError {
    fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
        write!(
            fmt,
            "Config validation failed with {} errors from {} validators",
            self.0, self.1
        )
    }
}

impl Error for ValidationError {}

struct Hooks<O, C> {
    config: Vec<Box<dyn FnMut(&O, &Arc<C>) + Send>>,
    config_loader: CfgLoader,
    config_mutators: Vec<Box<dyn FnMut(&mut C) + Send>>,
    config_validators: Vec<Box<dyn FnMut(&Arc<C>, &Arc<C>, &O) -> Result<Action, AnyError> + Send>>,
    sigs: HashMap<libc::c_int, Vec<Box<dyn FnMut() + Send>>>,
    singletons: HashSet<TypeId>,
    terminate: Vec<Box<dyn FnMut() + Send>>,
    guards: Vec<Box<dyn Any + Send>>,
    // There's terminated inside spirit itself, as atomic variable (for lock-less fast access). But
    // that is prone to races, so we keep a separate one here.
    terminated: bool,
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
            guards: Vec::new(),
            terminated: false,
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
/// By creating this (with the builder pattern), you start a background thread that keeps track of
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
/// method that contains callbacks or registers from within a callback, you'll get a deadlock.
///
/// # Interface
///
/// A lot of the methods on this are provided through the [`Extensible`] trait. You want to bring
/// that into scope (possibly with `use spirit::prelude::*`) and you want to have a look at the
/// methods on that trait.
///
/// # Examples
///
/// ```rust
/// use spirit::{Empty, Spirit};
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
    // Note: we ignore poisoning here. If one of the hooks fail, we do continue on purpose.
    hooks: Mutex<Hooks<O, C>>,
    around_hooks: Mutex<Vec<Box<dyn HookWrapper>>>,
    // TODO: Mode selection for directories
    opts: O,
    terminate: AtomicBool,
    autojoin_bg_thread: AtomicUsize,
    signals: Option<SigHandle>,
    bg_thread: Mutex<Option<JoinHandle<()>>>,
}

impl<O, C> Spirit<O, C>
where
    C: DeserializeOwned + Send + Sync,
    O: StructOpt,
{
    /// A constructor of the [`Builder`] with default initial config.
    ///
    /// Before the application successfully loads the first config, there still needs to be
    /// something (passed, for example, to validation callbacks) This puts the default value in
    /// there.
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> Builder<O, C>
    where
        C: Default,
    {
        Spirit::with_initial_config(C::default())
    }

    /// Similar to [`new`][Spirit::new], but with specific initial config value
    pub fn with_initial_config(config: C) -> Builder<O, C> {
        Builder {
            autojoin_bg_thread: Autojoin::TerminateAndJoin,
            before_bodies: Vec::new(),
            before_config: Vec::new(),
            body_wrappers: Vec::new(),
            config,
            config_loader: CfgBuilder::new(),
            config_hooks: Vec::new(),
            config_mutators: Vec::new(),
            config_validators: Vec::new(),
            around_hooks: Vec::new(),
            opts: PhantomData,
            sig_hooks: HashMap::new(),
            singletons: HashSet::new(),
            terminate_hooks: Vec::new(),
            guards: Vec::new(),
        }
    }

    /// Access the parsed command line.
    ///
    /// This gives the access to the command line options structure. The content doesn't
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
    /// # Examples
    ///
    /// ```rust
    /// use spirit::{Empty, Spirit};
    /// use spirit::prelude::*;
    ///
    /// let app = Spirit::<Empty, Empty>::new()
    ///     .build(false)
    ///     .unwrap();
    ///
    /// let spirit = app.spirit();
    ///
    /// let old_config = spirit.config();
    /// # drop(old_config);
    /// ```
    ///
    /// # Notes
    ///
    /// It is also possible to hook an external configuration storage into [`Spirit`] (or
    /// [`Builder`]) through the
    /// [`cfg_store`](https://docs.rs/spirit-cfg-helpers/0.3.*/spirit_cfg_helpers/fn.cfg_store.html)
    /// extension.
    pub fn config(&self) -> Arc<C> {
        self.config.load_full()
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
    /// [`terminate`][Spirit::terminate] from any callback as that would lead to a deadlock.
    pub fn config_reload(&self) -> Result<(), AnyError> {
        self.config_reload_with_wrapper(|inner| inner())
    }

    fn config_reload_with_wrapper<W>(&self, mut wrapper: W) -> Result<(), AnyError>
    where
        W: for<'a> FnMut(Box<dyn FnOnce() + 'a>),
    {
        let mut new = self.load_config().context("Failed to load configuration")?;
        // The lock here is across the whole processing, to avoid potential races in logic
        // processing. This makes writing the hooks correctly easier.
        let mut hooks = self.hooks.lock().unwrap_or_else(PoisonError::into_inner);
        debug!("Running {} config mutators", hooks.config_mutators.len());
        for m in &mut hooks.config_mutators {
            wrapper(Box::new(|| m(&mut new)));
        }
        let new = Arc::new(new);
        let old = self.config.load();
        debug!(
            "Running {} config validators",
            hooks.config_validators.len()
        );
        let mut errors = 0;
        let mut failed_validators = 0;
        let mut actions = Vec::with_capacity(hooks.config_validators.len());
        for v in hooks.config_validators.iter_mut() {
            let mut result = None;
            wrapper(Box::new(|| {
                result = Some(v(&old, &new, &self.opts));
            }));
            match result.expect("Hook wrapper didn't call inner") {
                Ok(ac) => actions.push(ac),
                Err(e) => {
                    failed_validators += 1;
                    match e.downcast::<MultiError>() {
                        Ok(e) => {
                            error!("{}", e);
                            errors += e.errors.len();
                            for e in e.errors {
                                crate::log_error!(multi Error, e);
                            }
                        }
                        Err(e) => {
                            errors += 1;
                            crate::log_error!(multi Error, e);
                        }
                    }
                }
            }
        }

        if errors == 0 {
            debug!("Validation successful, switching to new config");
            for a in actions {
                wrapper(Box::new(|| a.run(true)));
            }
        } else {
            debug!("Rolling back validation attempt");
            for a in actions {
                wrapper(Box::new(|| a.run(false)));
            }
            return Err(ValidationError(errors, failed_validators).into());
        }

        // Once everything is validated, switch to the new config
        self.config.store(Arc::clone(&new));
        debug!("Running {} post-configuration hooks", hooks.config.len());
        for hook in &mut hooks.config {
            wrapper(Box::new(|| hook(&self.opts, &new)));
        }
        debug!("Configuration reloaded");
        Ok(())
    }

    /// Is the application in the shutdown phase?
    ///
    /// This can be used if the daemon does some kind of periodic work, every loop it can check if
    /// the application should shut down.
    ///
    /// The other option is to hook into [`on_terminate`][Extensible::on_terminate]
    /// and shut things down (like drop some futures and make the tokio event loop empty).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::thread;
    /// use std::time::Duration;
    ///
    /// use spirit::{Empty, Spirit};
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
    /// * Sets the [`is_terminated`][Spirit::is_terminated] flag is set.
    /// * Drops all callbacks from spirit. This allows destruction/termination of parts of program
    ///   by dropping remote handles or similar things.
    /// * The background thread terminates.
    ///
    /// Note that it is still up to the rest of your application to terminate as a result of this.
    ///
    /// # Warning
    ///
    /// The Spirit guarantees only one callback runs at a time. That means you can't call this from
    /// within a callback (it would lead to deadlock).
    pub fn terminate(&self) {
        debug!("Running termination hooks");
        if let Some(signals) = &self.signals {
            signals.close();
        }
        let mut hooks = self.hooks.lock().unwrap_or_else(PoisonError::into_inner);
        // Take out the terminate hooks out first, so they are not called multiple times even in
        // case of panic.
        let mut term_hooks = mem::take(&mut hooks.terminate);
        for hook in &mut term_hooks {
            hook();
        }
        self.terminate.store(true, Ordering::Relaxed);
        // Get rid of all other hooks too. This drops any variables held by the closures,
        // potentially shutting down things than need to be shut down. But we need to keep the
        // guards (until the end of the spirit lifetime) and the singletons (so we don't register
        // something a second time in some corner case when it didn't notice it already
        // terminated).

        // Take everything out
        let mut old_hooks: Hooks<_, _> = mem::take(&mut hooks);
        // Return the interesting things back
        mem::swap(&mut old_hooks.guards, &mut hooks.guards);
        mem::swap(&mut old_hooks.singletons, &mut hooks.singletons);
        hooks.terminated = true;
        // Unlock before dropping all the hooks. Has been observed to deadlock at occasions.
        drop(hooks);
        trace!("Dropping all hooks");
        // Now we drop all the hooks and all that
    }

    fn background(&self, signals: &mut Signals) {
        debug!("Starting background processing");
        'forever: for info in signals {
            let name = get_proc_name(info.process.as_ref().map(|p| p.pid));
            info!("Received signal {:?}{}", info, name);
            let term = match info.signal {
                libc::SIGHUP => {
                    let _ = error::log_errors(module_path!(), || {
                        self.config_reload_with_wrapper(|inner| self.wrap_around_hooks(inner))
                    });
                    false
                }
                libc::SIGTERM | libc::SIGINT | libc::SIGQUIT => {
                    self.wrap_around_hooks(|| self.terminate());
                    true
                }
                // Some other signal, only for the hook benefit
                _ => false,
            };

            let mut lock = self.hooks.lock().unwrap_or_else(PoisonError::into_inner);

            if let Some(hooks) = lock.sigs.get_mut(&info.signal) {
                self.wrap_around_hooks(|| {
                    for hook in hooks {
                        hook();
                    }
                });
            }

            if term {
                break 'forever;
            }
        }
        debug!("Terminating the background thread");
    }

    fn load_config(&self) -> Result<C, AnyError> {
        self.hooks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .config_loader
            .load()
    }

    /// Waits for the background thread to terminate.
    ///
    /// The background thread terminates after a call to [`terminate`] or after a termination
    /// signal has been received.
    ///
    /// If the thread hasn't started or joined already, this returns right away.
    ///
    /// [`terminate`]: Spirit::terminate
    pub fn join_bg_thread(&self) {
        let handle = self
            .bg_thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(handle) = handle {
            handle
                .join()
                .expect("Spirit BG thread handles panics, shouldn't panic itself");
        }
    }

    pub(crate) fn maybe_autojoin_bg_thread(&self) {
        const TERM_JOIN: usize = Autojoin::TerminateAndJoin as _;
        const JOIN: usize = Autojoin::Join as _;
        const ABANDON: usize = Autojoin::Abandon as _;
        match self.autojoin_bg_thread.load(Ordering::Relaxed) {
            TERM_JOIN => {
                self.terminate();
                self.join_bg_thread();
            }
            JOIN => self.join_bg_thread(),
            ABANDON => (),
            val => unreachable!("Invalid autojoin variant: {}", val),
        }
    }

    fn wrap_around_hooks<R, I>(&self, inner: I) -> R
    where
        I: FnOnce() -> R,
    {
        let mut wrappers = self
            .around_hooks
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        wrap_around_hooks(&mut wrappers, inner)
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

    fn before_config<F>(self, cback: F) -> Result<Self, AnyError>
    where
        F: FnOnce(&Self::Config, &Self::Opts) -> Result<(), AnyError> + Send + 'static,
    {
        trace!("Running just added before_config");
        cback(&self.config(), self.cmd_opts())?;
        Ok(self)
    }

    fn config_validator<F>(self, mut f: F) -> Result<Self, AnyError>
    where
        F: FnMut(&Arc<C>, &Arc<C>, &O) -> Result<Action, AnyError> + Send + 'static,
    {
        trace!("Adding config validator at runtime");
        let mut hooks = self.hooks.lock().unwrap_or_else(PoisonError::into_inner);
        if !hooks.terminated {
            let cfg = self.config();
            f(&cfg, &cfg, self.cmd_opts())?.run(true);
            hooks.config_validators.push(Box::new(f));
        }
        Ok(self)
    }

    fn config_mutator<F>(self, f: F) -> Self
    where
        F: FnMut(&mut C) + Send + 'static,
    {
        trace!("Adding config mutator at runtime");
        let mut hooks = self.hooks.lock().unwrap_or_else(PoisonError::into_inner);
        if !hooks.terminated {
            hooks.config_mutators.push(Box::new(f));
        }
        self
    }

    fn on_config<F: FnMut(&O, &Arc<C>) + Send + 'static>(self, mut hook: F) -> Self {
        trace!("Adding config hook at runtime");
        let mut hooks = self.hooks.lock().unwrap_or_else(PoisonError::into_inner);
        if !hooks.terminated {
            hook(self.cmd_opts(), &self.config());
            hooks.config.push(Box::new(hook));
        }
        self
    }

    fn on_signal<F>(self, signal: libc::c_int, hook: F) -> Result<Self, AnyError>
    where
        F: FnMut() + Send + 'static,
    {
        let signals = self
            .signals
            .as_ref()
            .expect("Signals thread disabled by caller");
        trace!("Adding signal hook at runtime");
        signals.add_signal(signal)?;
        let mut hooks = self.hooks.lock().unwrap_or_else(PoisonError::into_inner);
        if !hooks.terminated {
            hooks
                .sigs
                .entry(signal)
                .or_insert_with(Vec::new)
                .push(Box::new(hook));
        }
        Ok(self)
    }

    fn on_terminate<F: FnOnce() + Send + 'static>(self, hook: F) -> Self {
        trace!("Running termination hook at runtime");
        let mut hook = Some(hook);
        let mut hooks = self.hooks.lock().unwrap_or_else(PoisonError::into_inner);
        if hooks.terminated {
            (hooks.terminate.last_mut().unwrap())();
        } else {
            hooks.terminate.push(Box::new(move || {
                (hook.take().expect("Termination hook called multiple times"))()
            }));
        }
        self
    }

    fn with<E>(self, ext: E) -> Result<Self::Ok, AnyError>
    where
        E: Extension<Self>,
    {
        ext.apply(self)
    }

    fn singleton<T: 'static>(&mut self) -> bool {
        self.hooks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .singletons
            .insert(TypeId::of::<T>())
    }

    fn run_before<B>(self, body: B) -> Result<Self, AnyError>
    where
        B: FnOnce(&Arc<Spirit<Self::Opts, Self::Config>>) -> Result<(), AnyError> + Send + 'static,
    {
        body(self).map(|()| self)
    }

    fn run_around<W>(self, _wrapper: W) -> Result<Self, AnyError>
    where
        W: FnOnce(&Arc<Spirit<Self::Opts, Self::Config>>, InnerBody) -> Result<(), AnyError>
            + Send
            + 'static,
    {
        panic!("Wrapping body while already running is not possible, move this to the builder (see https://docs.rs/spirit/*/extension/trait.Extensible.html#method.run_around");
    }

    fn around_hooks<W>(self, wrapper: W) -> Self
    where
        W: HookWrapper,
    {
        trace!("Adding around-hook at runtime");
        self.around_hooks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Box::new(wrapper));
        self
    }

    fn with_singleton<T>(mut self, singleton: T) -> Result<Self, AnyError>
    where
        T: Extension<Self::Ok> + 'static,
    {
        if self.singleton::<T>() {
            self.with(singleton)
        } else {
            Ok(self)
        }
    }

    fn keep_guard<G: Any + Send>(self, guard: G) -> Self {
        self.hooks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .guards
            .push(Box::new(guard));
        self
    }

    fn autojoin_bg_thread(self, autojoin: Autojoin) -> Self {
        self.autojoin_bg_thread
            .store(autojoin as usize, Ordering::Relaxed);
        self
    }
}

/// The builder of [`Spirit`].
///
/// This is returned by the [`Spirit::new`].
///
/// # Interface
///
/// Most of the methods are available through the following traits. You want to bring them into
/// scope (possibly by `use spirit::prelude::*`) and look at their methods.
///
/// * [`ConfigBuilder`] to specify how configuration should be loaded.
/// * [`Extensible`] to register callbacks.
/// * [`SpiritBuilder`] to turn the builder into [`Spirit`].
#[must_use = "The builder is inactive without calling `run` or `build`"]
pub struct Builder<O = Empty, C = Empty> {
    autojoin_bg_thread: Autojoin,
    before_bodies: Vec<SpiritBody<O, C>>,
    before_config: Vec<Box<dyn FnMut(&C, &O) -> Result<(), AnyError> + Send>>,
    body_wrappers: Vec<Wrapper<O, C>>,
    config: C,
    config_loader: CfgBuilder,
    config_hooks: Vec<Box<dyn FnMut(&O, &Arc<C>) + Send>>,
    config_mutators: Vec<Box<dyn FnMut(&mut C) + Send>>,
    config_validators: Vec<Box<dyn FnMut(&Arc<C>, &Arc<C>, &O) -> Result<Action, AnyError> + Send>>,
    around_hooks: Vec<Box<dyn HookWrapper>>,
    opts: PhantomData<O>,
    sig_hooks: HashMap<libc::c_int, Vec<Box<dyn FnMut() + Send>>>,
    singletons: HashSet<TypeId>,
    terminate_hooks: Vec<Box<dyn FnMut() + Send>>,
    guards: Vec<Box<dyn Any + Send>>,
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

    fn config_defaults<D: Into<String>>(self, config: D) -> Self {
        Self {
            config_loader: self.config_loader.config_defaults(config),
            ..self
        }
    }

    fn config_env<E: Into<String>>(self, env: E) -> Self {
        Self {
            config_loader: self.config_loader.config_env(env),
            ..self
        }
    }

    fn config_filter<F: FnMut(&Path) -> bool + Send + 'static>(self, filter: F) -> Self {
        Self {
            config_loader: self.config_loader.config_filter(filter),
            ..self
        }
    }

    fn warn_on_unused(self, warn: bool) -> Self {
        Self {
            config_loader: self.config_loader.warn_on_unused(warn),
            ..self
        }
    }
}

impl<O, C> Extensible for Builder<O, C> {
    type Opts = O;
    type Config = C;
    type Ok = Self;
    const STARTED: bool = false;

    fn before_config<F>(mut self, cback: F) -> Result<Self, AnyError>
    where
        F: FnOnce(&C, &O) -> Result<(), AnyError> + Send + 'static,
    {
        let mut cback = Some(cback);
        let cback = move |conf: &C, opts: &O| (cback.take().unwrap())(conf, opts);
        self.before_config.push(Box::new(cback));
        Ok(self)
    }

    fn config_validator<F>(self, f: F) -> Result<Self, AnyError>
    where
        F: FnMut(&Arc<C>, &Arc<C>, &O) -> Result<Action, AnyError> + Send + 'static,
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

    fn on_signal<F>(self, signal: libc::c_int, hook: F) -> Result<Self, AnyError>
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

    fn on_terminate<F: FnOnce() + Send + 'static>(self, hook: F) -> Self {
        let mut hook = Some(hook);
        let mut hooks = self.terminate_hooks;
        hooks.push(Box::new(move || {
            (hook.take().expect("Termination hook called more than once"))();
        }));
        Self {
            terminate_hooks: hooks,
            ..self
        }
    }

    fn with<E>(self, ext: E) -> Result<Self::Ok, AnyError>
    where
        E: Extension<Self>,
    {
        ext.apply(self)
    }

    fn singleton<T: 'static>(&mut self) -> bool {
        self.singletons.insert(TypeId::of::<T>())
    }

    fn run_before<B>(mut self, body: B) -> Result<Self, AnyError>
    where
        B: FnOnce(&Arc<Spirit<O, C>>) -> Result<(), AnyError> + Send + 'static,
    {
        self.before_bodies.push(Box::new(body));
        Ok(self)
    }

    fn run_around<W>(mut self, wrapper: W) -> Result<Self, AnyError>
    where
        W: FnOnce(&Arc<Spirit<O, C>>, InnerBody) -> Result<(), AnyError> + Send + 'static,
    {
        self.body_wrappers.push(Box::new(wrapper));
        Ok(self)
    }

    fn around_hooks<W>(mut self, wrapper: W) -> Self
    where
        W: HookWrapper,
    {
        self.around_hooks.push(Box::new(wrapper));
        self
    }

    fn with_singleton<T>(mut self, singleton: T) -> Result<Self, AnyError>
    where
        T: Extension<Self::Ok> + 'static,
    {
        if self.singleton::<T>() {
            self.with(singleton)
        } else {
            Ok(self)
        }
    }

    fn keep_guard<G: Any + Send>(mut self, guard: G) -> Self {
        self.guards.push(Box::new(guard));
        self
    }

    fn autojoin_bg_thread(self, autojoin: Autojoin) -> Self {
        Self {
            autojoin_bg_thread: autojoin,
            ..self
        }
    }
}

/// An interface to turn the spirit [`Builder`] into a [`Spirit`] and possibly run it.
///
/// It is a trait to support a little trick. The [`run`][SpiritBuilder::run] method has the ability
/// to handle errors by logging them and then terminating the application with a non-zero exit
/// code. It is convenient to include even the setup errors in this logging.
///
/// Therefore, the trait is implemented both on [`Builder`] and `Result<Builder, AnyError>` (and the
/// other traits on [`Builder`] also support this trick). That way all the errors can be just
/// transparently gathered and handled uniformly, without any error handling on the user side.
///
/// # Examples
///
/// ```rust
/// use spirit::{Empty, Spirit};
/// use spirit::prelude::*;
///
/// // This returns Builder
/// Spirit::<Empty, Empty>::new()
///     // This method returns Result<Builder, AnyError>, but we don't handle the possible error with
///     // ?.
///     .run_before(|_| Ok(()))
///     // The run lives both on Builder and Result<Builder, AnyError>. Here we call the latter.
///     // If we get an error from above, the body here isn't run at all, but the error is handled
///     // similarly as any errors from within the body.
///     .run(|_| Ok(()))
/// ```
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
    /// This version returns the [`App`] (or error) and error handling is up to the caller. If you
    /// want spirit to take care of nice error logging (even for your application's top level
    /// errors), use [`run`] instead.
    ///
    /// # Warning
    ///
    /// If asked to go to background (when you're using the
    /// [`spirit-daemonize`](https://lib.rs/crates/spirit-daemonize) crate, this uses `fork`.
    /// Therefore, start any threads after you call `build` (or from within [`run`]),
    /// or you'll lose them ‒ only the thread doing fork is preserved across it.
    ///
    /// [`run`]: SpiritBuilder::run
    ///
    /// # Panics
    ///
    /// This will panic if `background_thread` is set to `false` and there are registered signals.
    // TODO: The new return value
    fn build(self, background_thread: bool) -> Result<App<Self::Opts, Self::Config>, AnyError>;

    /// Build the spirit and run the application, handling all relevant errors.
    ///
    /// In case an error happens (either when creating the Spirit, or returned by the callback),
    /// the errors are logged (either to the place where logs are sent to in configuration, or to
    /// stderr if the error happens before logging is initialized ‒ for example if configuration
    /// can't be read). The application then terminates with failure exit code.
    ///
    /// This mostly just wraps whatever the [`App::run_term`] does, but also handles the errors
    /// that already happened on the [`Builder`].
    ///
    /// ```rust
    /// use std::thread;
    /// use std::time::Duration;
    ///
    /// use spirit::{Empty, Spirit};
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
        B: FnOnce(&Arc<Spirit<Self::Opts, Self::Config>>) -> Result<(), AnyError> + Send + 'static;
}

impl<O, C> SpiritBuilder for Builder<O, C>
where
    Self::Config: DeserializeOwned + Send + Sync + 'static,
    Self::Opts: StructOpt + Sync + Send + 'static,
{
    fn build(mut self, background_thread: bool) -> Result<App<O, C>, AnyError> {
        debug!("Building the spirit");
        let (opts, loader) = self.config_loader.build::<Self::Opts>();
        let wrappers = &mut self.around_hooks;
        let before_configs = &mut self.before_config;
        let config = &self.config;
        wrap_around_hooks(wrappers, || -> Result<(), AnyError> {
            for before_config in before_configs {
                before_config(config, &opts).context("The before-config phase failed")?;
            }
            Ok(())
        })?;
        let interesting_signals = self
            .sig_hooks
            .keys()
            .chain(&[libc::SIGHUP, libc::SIGTERM, libc::SIGQUIT, libc::SIGINT])
            .cloned()
            .collect::<HashSet<_>>(); // Eliminate duplicates
        let config = ArcSwap::from(Arc::from(self.config));
        let mut signals = if background_thread {
            Some(Signals::new(interesting_signals)?)
        } else {
            assert!(
                self.sig_hooks.is_empty(),
                "Registered signals; now starting without a signal thread",
            );
            None
        };
        let sig_handle = signals.as_ref().map(|s| s.handle());
        let spirit = Spirit {
            autojoin_bg_thread: AtomicUsize::new(self.autojoin_bg_thread as _),
            config,
            hooks: Mutex::new(Hooks {
                config: self.config_hooks,
                config_loader: loader,
                config_mutators: self.config_mutators,
                config_validators: self.config_validators,
                sigs: self.sig_hooks,
                singletons: self.singletons,
                terminate: self.terminate_hooks,
                terminated: false,
                guards: self.guards,
            }),
            around_hooks: Mutex::new(self.around_hooks),
            opts,
            terminate: AtomicBool::new(false),
            signals: sig_handle,
            bg_thread: Mutex::new(None),
        };
        spirit
            .config_reload_with_wrapper(|inner| spirit.wrap_around_hooks(inner))
            .context("Problem loading the initial configuration")?;
        let spirit = Arc::new(spirit);
        if background_thread {
            let spirit_bg = Arc::clone(&spirit);
            let handle = thread::Builder::new()
                .name("spirit".to_owned())
                .spawn(move || {
                    loop {
                        // Note: we run a bunch of callbacks inside the service thread. We restart
                        // the thread if it fails.
                        let run =
                            AssertUnwindSafe(|| spirit_bg.background(signals.as_mut().unwrap()));
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
            *spirit
                .bg_thread
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(handle);
        }
        debug!(
            "Building bodies from {} before-bodies and {} wrappers",
            self.before_bodies.len(),
            self.body_wrappers.len()
        );
        let spirit_body = Arc::clone(&spirit);
        let bodies = self.before_bodies;
        let inner = move || {
            for body in bodies {
                body(&spirit_body)?;
            }
            Ok(())
        };
        let body_wrappers = self.body_wrappers;
        let inner = Box::new(inner);
        let spirit_body = Arc::clone(&spirit);
        let mut wrapped = Box::new(|inner: InnerBody| inner()) as WrapBody;
        for wrapper in body_wrappers.into_iter().rev() {
            // TODO: Can we get rid of this clone?
            let spirit = Arc::clone(&spirit_body);
            let applied = move |inner: InnerBody| wrapper(&spirit, inner);
            wrapped = Box::new(applied) as WrapBody;
        }
        Ok(App::new(spirit, inner, wrapped))
    }

    fn run<B: FnOnce(&Arc<Spirit<O, C>>) -> Result<(), AnyError> + Send + 'static>(self, body: B) {
        Ok(self).run(body);
    }
}

impl<O, C> SpiritBuilder for Result<Builder<O, C>, AnyError>
where
    Self::Config: DeserializeOwned + Send + Sync + 'static,
    Self::Opts: StructOpt + Sync + Send + 'static,
{
    fn build(self, background_thread: bool) -> Result<App<O, C>, AnyError> {
        self.and_then(|b| b.build(background_thread))
    }
    fn run<B: FnOnce(&Arc<Spirit<O, C>>) -> Result<(), AnyError> + Send + 'static>(self, body: B) {
        let result = error::log_errors("top-level", || {
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
        spirit.on_terminate(|| ()).on_config(|_opts, _cfg| ());
    }
}
