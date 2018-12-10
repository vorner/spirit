#![doc(
    html_root_url = "https://docs.rs/spirit/0.2.7/spirit/",
    test(attr(deny(warnings)))
)]
#![allow(renamed_and_removed_lints)] // Until the clippy thing can be reasonably resolved
#![cfg_attr(feature = "cargo-clippy", allow(type_complexity))]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! A helper to create unix daemons.
//!
//! When writing a service (in the unix terminology, a daemon), there are two parts of the job. One
//! is the actual functionality of the service, the part that makes it different than all the other
//! services out there. And then there's the very boring part of turning the prototype
//! implementation into a well-behaved service and handling all the things expected of all of them.
//!
//! This crate is supposed to help with the latter. Before using, you should know the following:
//!
//! * This is an early version and while already (hopefully) useful, it is expected to expand and
//!   maybe change a bit in future versions. There are certainly parts of functionality I still
//!   haven't written and expect it to be rough around the edges.
//! * It is opinionated ‒ it comes with an idea about how a well-behaved daemon should look like
//!   and how to integrate that into your application. I simply haven't find a way to make it less
//!   opinionated yet and this helps to scratch my own itch, so it reflects what I needed. If you
//!   have use cases that you think should fall within the responsibilities of this crate and are
//!   not handled, you are of course welcome to open an issue (or even better, a pull request) on
//!   the repository ‒ it would be great if it scratched not only my own itch.
//! * It brings in a lot of dependencies. There will likely be features to turn off the unneeded
//!   parts, but for now, nobody made them yet.
//! * This supports unix-style daemons only *for now*. This is because I have no experience in how
//!   a service for different OS should look like. However, help in this area would be appreciated
//!   ‒ being able to write a single code and compile a cross-platform service with all the needed
//!   plumbing would indeed sound very useful.
//!
//! You can have a look at a [tutorial](https://vorner.github.io/2018/12/09/Spirit-Tutorial.html)
//! first before diving into the API documentation.
//!
//! # What the crate does and how
//!
//! To be honest, the crate doesn't bring much (or, maybe mostly none) of novelty functionality to
//! the table. It just takes other crates doing something useful and gluing them together to form
//! something most daemons want to do.
//!
//! By composing these things together the crate allows for cutting down on your own boilerplate
//! code around configuration handling, signal handling and command line arguments.
//!
//! Using the builder pattern, you create a singleton [`Spirit`] object. That one starts a
//! background thread that runs some callbacks configured previously when things happen.
//!
//! It takes two structs, one for command line arguments (using [`StructOpt`]) and another for
//! configuration (implementing [`serde`]'s [`Deserialize`], loaded using the [`config`] crate). It
//! enriches both to add common options, like configuration overrides on the command line and
//! logging into the configuration.
//!
//! The background thread listens to certain signals (like `SIGHUP`) using the [`signal-hook`] crate
//! and reloads the configuration when requested. It manages the logging backend to reopen on
//! `SIGHUP` and reflect changes to the configuration.
//!
//! [`Spirit`]: struct.Spirit.html
//! [`StructOpt`]: https://crates.io/crates/structopt
//! [`serde`]: https://crates.io/crates/serde
//! [`Deserialize`]: https://docs.rs/serde/*/serde/trait.Deserialize.html
//! [`config`]: https://crates.io/crates/config
//! [`signal-hook`]: https://crates.io/crates/signal-hook
//!
//! # Features
//!
//! There are several features that can tweak functionality. Currently, they are all *on* by
//! default, but they can be opted out of. All the other spirit crates depend only on the bare
//! minimum of features they need.
//!
//! * `ini`, `json`, `hjson`, `yaml`: support for given configuration formats.
//!
//! # Helpers
//!
//! It brings the idea of helpers. A helper is something that plugs a certain functionality into
//! the main crate, to cut down on some more specific boiler-plate code. These are usually provided
//! by other crates. To list some:
//!
//! * `spirit-daemonize`: Configuration and routines to go into background and be a nice daemon.
//! * `spirit-log`: Configuration of logging.
//! * `spirit-tokio`: Integrates basic tokio primitives ‒ auto-reconfiguration for TCP and UDP
//!   sockets and starting the runtime.
//! * `spirit-hyper`: Integrates the hyper web server.
//!
//! (Others will come over time)
//!
//! There are some general helpers in the [`helpers`](helpers/) module.
//!
//! # Examples
//!
//! ```rust
//! #[macro_use]
//! extern crate log;
//! #[macro_use]
//! extern crate serde_derive;
//! extern crate spirit;
//!
//! use std::time::Duration;
//! use std::thread;
//!
//! use spirit::{Empty, Spirit};
//!
//! #[derive(Debug, Default, Deserialize)]
//! struct Cfg {
//!     message: String,
//!     sleep: u64,
//! }
//!
//! static DEFAULT_CFG: &str = r#"
//! message = "hello"
//! sleep = 2
//! "#;
//!
//! fn main() {
//!     Spirit::<Empty, Cfg>::new()
//!         // Provide default values for the configuration
//!         .config_defaults(DEFAULT_CFG)
//!         // If the program is passed a directory, load files with these extensions from there
//!         .config_exts(&["toml", "ini", "json"])
//!         .on_terminate(|| debug!("Asked to terminate"))
//!         .on_config(|_opts, cfg| debug!("New config loaded: {:?}", cfg))
//!         // Run the closure, logging the error nicely if it happens (note: no error happens
//!         // here)
//!         .run(|spirit: &_| {
//!             while !spirit.is_terminated() {
//!                 let cfg = spirit.config(); // Get a new version of config every round
//!                 thread::sleep(Duration::from_secs(cfg.sleep));
//!                 info!("{}", cfg.message);
//! #               spirit.terminate(); // Just to make sure the doc-test terminates
//!             }
//!             Ok(())
//!         });
//! }
//! ```
//!
//! More complete examples can be found in the
//! [repository](https://github.com/vorner/spirit/tree/master/examples).
//!
//! # Added configuration and options
//!
//! ## Command line options
//!
//! * `config-override`: Override configuration value.
//!
//! Furthermore, it takes a list of paths ‒ both files and directories. They are loaded as
//! configuration files (the directories are examined and files in them ‒ the ones passing a
//! [`filter`](struct.Builder.html#method.config_files) ‒ are also loaded).
//!
//! # Common patterns
//!
//! TODO

extern crate arc_swap;
extern crate config;
#[macro_use]
extern crate failure;
extern crate fallible_iterator;
extern crate itertools;
extern crate libc;
#[macro_use]
extern crate log;
extern crate parking_lot;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate signal_hook;
// For some reason, this produces a warning about unused on nightly… but it is needed on stable
#[allow(unused_imports)]
#[macro_use]
extern crate structopt;

pub mod helpers;
pub mod utils;
pub mod validation;

use std::any::TypeId;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub use arc_swap::ArcSwap;
use arc_swap::Lease;
use config::{Config, Environment, File, FileFormat};
use failure::{Error, Fail, ResultExt};
use fallible_iterator::FallibleIterator;
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use signal_hook::iterator::Signals;
use structopt::clap::App;
use structopt::StructOpt;

use validation::{
    Error as ValidationError, Level as ValidationLevel, Results as ValidationResults,
};

#[deprecated = "Moved to spirit::utils"]
pub use utils::{key_val, log_error, log_errors, log_errors_named, MissingEquals};

#[derive(Debug, StructOpt)]
struct CommonOpts {
    /// Override specific config values.
    #[structopt(
        short = "C",
        long = "config-override",
        parse(try_from_str = "utils::key_val"),
        raw(number_of_values = "1")
    )]
    config_overrides: Vec<(String, String)>,

    /// Configuration files or directories to load.
    #[structopt(parse(from_os_str = "utils::absolute_from_os_str"))]
    configs: Vec<PathBuf>,
}

#[derive(Debug)]
struct OptWrapper<O> {
    common: CommonOpts,
    other: O,
}

// Unfortunately, StructOpt doesn't like flatten with type parameter
// (https://github.com/TeXitoi/structopt/issues/128). It is not even trivial to do, since some of
// the very important functions are *not* part of the trait. So we do it manually ‒ we take the
// type parameter's clap definition and add our own into it.
impl<O> StructOpt for OptWrapper<O>
where
    O: Debug + StructOpt,
{
    fn clap<'a, 'b>() -> App<'a, 'b> {
        CommonOpts::augment_clap(O::clap())
    }

    fn from_clap(matches: &::structopt::clap::ArgMatches) -> Self {
        OptWrapper {
            common: StructOpt::from_clap(matches),
            other: StructOpt::from_clap(matches),
        }
    }
}

/// An error returned whenever the user passes something not a file nor a directory as
/// configuration.
#[derive(Debug, Fail)]
#[fail(display = "Configuration path {:?} is not a file nor a directory", _0)]
pub struct InvalidFileType(PathBuf);

/// Returned if configuration path is missing.
#[derive(Debug, Fail)]
#[fail(display = "Configuration path {:?} does not exist", _0)]
pub struct MissingFile(PathBuf);

/// A struct that may be used when either configuration or command line options are not needed.
///
/// When the application doesn't need the configuration (in excess of the automatic part provided
/// by this library) or it doesn't need any command line options of its own, this struct can be
/// used to plug the type parameter.
///
/// Other places (eg. around helpers) may use this to plug a type parameter that isn't needed, do
/// nothing or something like that.
#[derive(
    Copy, Clone, Debug, Default, Deserialize, Eq, PartialEq, Hash, Ord, PartialOrd, StructOpt,
)]
pub struct Empty {}

struct Hooks<O, C> {
    config_filter: Box<FnMut(&Path) -> bool + Send>,
    config: Vec<Box<FnMut(&O, &Arc<C>) + Send>>,
    config_validators: Vec<Box<FnMut(&Arc<C>, &mut C, &O) -> ValidationResults + Send>>,
    sigs: HashMap<libc::c_int, Vec<Box<FnMut() + Send>>>,
    terminate: Vec<Box<FnMut() + Send>>,
}

impl<O, C> Default for Hooks<O, C> {
    fn default() -> Self {
        let no_filter = Box::new(|_: &_| false);
        Hooks {
            config_filter: no_filter,
            config: Vec::new(),
            config_validators: Vec::new(),
            sigs: HashMap::new(),
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
/// use spirit::{Empty, Spirit};
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
    config_files: Vec<PathBuf>,
    config_defaults: Option<String>,
    config_env: Option<String>,
    config_overrides: HashMap<String, String>,
    opts: O,
    terminate: AtomicBool,
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
    #[allow(unknown_lints, new_ret_no_self)]
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
            config_default_paths: Vec::new(),
            config_defaults: None,
            config_env: None,
            config_hooks: Vec::new(),
            config_filter: Box::new(|_| false),
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
    /// extern crate arc_swap;
    /// extern crate spirit;
    ///
    /// use arc_swap::Lease;
    /// use spirit::{Empty, Spirit};
    ///
    /// let (spirit, _, _) = Spirit::<Empty, Empty>::new()
    ///     .build(false)
    ///     .unwrap();
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
        let old = self.config.load();
        debug!("Running config validators");
        let mut results = hooks
            .config_validators
            .iter_mut()
            .map(|v| v(&old, &mut new, &self.opts))
            .fold(ValidationResults::new(), |mut acc, r| {
                acc.merge(r);
                acc
            });
        let new = Arc::new(new);
        for result in &results {
            match result.level() {
                ValidationLevel::Error => {
                    if let Some(e) = result.detailed_error() {
                        utils::log_error("configuration", e);
                    } else {
                        error!(target: "configuration", "{}", result.description());
                    }
                }
                ValidationLevel::Warning => {
                    warn!(target: "configuration", "{}", result.description());
                }
                ValidationLevel::Hint => {
                    info!(target: "configuration", "{}", result.description());
                }
                ValidationLevel::Nothing => (),
            }
        }
        if results.max_level() == Some(ValidationLevel::Error) {
            error!(target: "configuration", "Refusing new configuration due to errors");
            for r in &mut results.0 {
                if let Some(abort) = r.on_abort.as_mut() {
                    abort();
                }
            }
            return Err(ValidationError::from(results).into());
        }
        debug!("Validation successful, installing new config");
        for r in &mut results.0 {
            if let Some(success) = r.on_success.as_mut() {
                success();
            }
        }
        // Once everything is validated, switch to the new config
        self.config.store(Arc::clone(&new));
        debug!("Running post-configuration hooks");
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
    /// use spirit::{Empty, Spirit};
    ///
    /// let (spirit, _, _) = Spirit::<Empty, Empty>::new()
    ///     .build(false)
    ///     .unwrap();
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
        debug!("Loading configuration");
        let mut config = Config::new();
        // To avoid problems with trying to parse without any configuration present (it would
        // complain that it found unit and whatever the config was is expected instead).
        config.merge(File::from_str("", FileFormat::Toml))?;
        if let Some(ref defaults) = self.config_defaults {
            trace!("Loading config defaults");
            config
                .merge(File::from_str(defaults, FileFormat::Toml))
                .context("Failed to read defaults")?;
        }
        for path in &self.config_files {
            if path.is_file() {
                trace!("Loading config file {:?}", path);
                config
                    .merge(File::from(path as &Path))
                    .with_context(|_| format!("Failed to load config file {:?}", path))?;
            } else if path.is_dir() {
                trace!("Scanning directory {:?}", path);
                let mut lock = self.hooks.lock();
                let mut filter = &mut lock.config_filter;
                // Take all the file entries passing the config file filter, handling errors on the
                // way.
                let mut files = fallible_iterator::convert(path.read_dir()?)
                    .and_then(|entry| -> Result<Option<PathBuf>, std::io::Error> {
                        let path = entry.path();
                        let meta = path.symlink_metadata()?;
                        if meta.is_file() && (filter)(&path) {
                            Ok(Some(path))
                        } else {
                            trace!("Skipping {:?}", path);
                            Ok(None)
                        }
                    })
                    .filter_map(|path| path)
                    .collect::<Vec<_>>()?;
                // Traverse them sorted.
                files.sort();
                for file in files {
                    trace!("Loading config file {:?}", file);
                    config
                        .merge(File::from(&file as &Path))
                        .with_context(|_| format!("Failed to load config file {:?}", file))?;
                }
            } else if path.exists() {
                bail!(InvalidFileType(path.to_owned()));
            } else {
                bail!(MissingFile(path.to_owned()));
            }
        }
        if let Some(env_prefix) = self.config_env.as_ref() {
            trace!("Loading config from environment {}", env_prefix);
            config
                .merge(Environment::with_prefix(env_prefix).separator("_"))
                .context("Failed to include environment in config")?;
        }
        for (ref key, ref value) in &self.config_overrides {
            trace!("Config override {} => {}", key, value);
            config.set(*key, *value as &str).with_context(|_| {
                format!("Failed to push override {}={} into config", key, value)
            })?;
        }
        let result = config
            .try_into()
            .context("Failed to decode configuration")?;
        Ok(result)
    }
}

trait Body<Param>: Send {
    fn run(&mut self, Param) -> Result<(), Error>;
}

impl<F: FnOnce(Param) -> Result<(), Error> + Send, Param> Body<Param> for Option<F> {
    fn run(&mut self, param: Param) -> Result<(), Error> {
        (self.take().expect("Body called multiple times"))(param)
    }
}

/// A workaround type for `Box<FnOnce() -> Result<(), Error>`.
///
/// Since it is not possible to use the aforementioned type in any meaningful way in Rust yet, this
/// works around the problem. The type has a [`run`](#method.run) method which does the same thing.
///
/// This is passed as parameter to the [`body_wrapper`](struct.Builder.html#method.body_wrapper).
///
/// It is also returned as part of the [`build`](struct.Builder.html#method.build)'s result.
pub struct InnerBody(Box<Body<()>>);

impl InnerBody {
    /// Run the body.
    pub fn run(mut self) -> Result<(), Error> {
        self.0.run(())
    }
}

/// A wrapper around a body.
///
/// These are essentially boxed closures submitted by
/// [`body_wrapper`](struct.Builder.html#method.body_wrapper) (or all of them folded together), but
/// in a form that is usable (in contrast to `Box<FnOnce(InnerBody) -> Result(), Error>`). It
/// is part of the return value of [`build`](struct.Builder.html#method.build) and the caller
/// should call it eventually.
pub struct WrapBody(Box<Body<InnerBody>>);

impl WrapBody {
    /// Call the closure inside.
    pub fn run(mut self, inner: InnerBody) -> Result<(), Error> {
        self.0.run(inner)
    }
}

type Wrapper<O, C> = Box<for<'a> Body<(&'a Arc<Spirit<O, C>>, InnerBody)>>;
type SpiritBody<O, C> = Box<for<'a> Body<&'a Arc<Spirit<O, C>>>>;

/// The builder of [`Spirit`](struct.Spirit.html).
///
/// This is returned by the [`Spirit::new`](struct.Spirit.html#new).
#[must_use = "The builder is inactive without calling `run` or `build`"]
pub struct Builder<O = Empty, C = Empty> {
    before_bodies: Vec<SpiritBody<O, C>>,
    before_config: Vec<Box<FnMut(&O) -> Result<(), Error> + Send>>,
    body_wrappers: Vec<Wrapper<O, C>>,
    config: C,
    config_default_paths: Vec<PathBuf>,
    config_defaults: Option<String>,
    config_env: Option<String>,
    config_hooks: Vec<Box<FnMut(&O, &Arc<C>) + Send>>,
    config_filter: Box<FnMut(&Path) -> bool + Send>,
    config_validators: Vec<Box<FnMut(&Arc<C>, &mut C, &O) -> ValidationResults + Send>>,
    opts: PhantomData<O>,
    sig_hooks: HashMap<libc::c_int, Vec<Box<FnMut() + Send>>>,
    singletons: HashSet<TypeId>,
    terminate_hooks: Vec<Box<FnMut() + Send>>,
}

impl<O, C> Builder<O, C>
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
{
    /// Add a closure run before the main body.
    ///
    /// The [`run`](#method.run) will first execute all closures submitted through this method
    /// before running the real body. They are run in the order of submissions.
    ///
    /// The purpose of this is mostly integration with helpers ‒ they often need some last minute
    /// preparation.
    ///
    /// In case of using only build, the bodies are composed into one object and returned as part
    /// of the result (the inner body).
    ///
    /// # Errors
    ///
    /// If any of the before-bodies in the chain return an error, the processing ends there and the
    /// error is returned right away.
    ///
    /// # Examples
    ///
    /// ```
    /// # use spirit::{Empty, Spirit};
    /// Spirit::<Empty, Empty>::new()
    ///     .before_body(|_spirit| {
    ///         println!("Run first");
    ///         Ok(())
    ///     }).run(|_spirit| {
    ///         println!("Run second");
    ///         Ok(())
    ///     });
    /// ```
    pub fn before_body<B>(mut self, body: B) -> Self
    where
        B: FnOnce(&Arc<Spirit<O, C>>) -> Result<(), Error> + Send + 'static,
    {
        self.before_bodies.push(Box::new(Some(body)));
        self
    }

    /// A callback that is run after the building started and the command line is parsed, but even
    /// before the first configuration is loaded.
    ///
    /// If the callback returns an error, the building is aborted.
    pub fn before_config<F>(mut self, cback: F) -> Self
    where
        F: FnOnce(&O) -> Result<(), Error> + Send + 'static,
    {
        let mut cback = Some(cback);
        let cback = move |opts: &O| (cback.take().unwrap())(opts);
        self.before_config.push(Box::new(cback));
        self
    }

    /// Wrap the body run by the [`run`](#method.run) into this closure.
    ///
    /// The inner body is passed as an object with a [`run`](struct.InnerBody.html#method.run)
    /// method, not as a closure, due to a limitation around boxed `FnOnce`.
    ///
    /// It is expected the wrapper executes the inner body as part of itself and propagates any
    /// returned error.
    ///
    /// In case of multiple wrappers, the ones submitted later on are placed inside the sooner
    /// ones ‒ the first one is the outermost.
    ///
    /// In case of using only [`build`](#method.build), all the wrappers composed together are
    /// returned as part of the result.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use spirit::{Empty, Spirit};
    /// Spirit::<Empty, Empty>::new()
    ///     .body_wrapper(|_spirit, inner| {
    ///         println!("Run first");
    ///         inner.run()?;
    ///         println!("Run third");
    ///         Ok(())
    ///     }).run(|_spirit| {
    ///         println!("Run second");
    ///         Ok(())
    ///     });
    /// ```
    pub fn body_wrapper<W>(mut self, wrapper: W) -> Self
    where
        W: FnOnce(&Arc<Spirit<O, C>>, InnerBody) -> Result<(), Error> + Send + 'static,
    {
        let wrapper = move |(spirit, inner): (&_, _)| wrapper(spirit, inner);
        self.body_wrappers.push(Box::new(Some(wrapper)));
        self
    }

    /// Sets the configuration paths in case the user doesn't provide any.
    ///
    /// This replaces any previously set default paths. If none are specified and the user doesn't
    /// specify any either, no config is loaded (but it is not an error, simply the defaults will
    /// be used, if available).
    pub fn config_default_paths<P, I>(self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        let paths = paths.into_iter().map(Into::into).collect();
        Self {
            config_default_paths: paths,
            ..self
        }
    }

    /// Specifies the default configuration.
    ///
    /// This „loads“ the lowest layer of the configuration from the passed string. The expected
    /// format is TOML.
    pub fn config_defaults<D: Into<String>>(self, config: D) -> Self {
        Self {
            config_defaults: Some(config.into()),
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
    /// extern crate failure;
    /// #[macro_use]
    /// extern crate serde_derive;
    /// extern crate spirit;
    ///
    /// use failure::Error;
    /// use spirit::{Empty, Spirit};
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
    pub fn config_env<E: Into<String>>(self, env: E) -> Self {
        Self {
            config_env: Some(env.into()),
            ..self
        }
    }

    /// Configures a config dir filter for a single extension.
    ///
    /// Sets the config directory filter (see [`config_filter`](#method.config_filter)) to one
    /// matching this single extension.
    pub fn config_ext<E: Into<OsString>>(self, ext: E) -> Self {
        let ext = ext.into();
        Self {
            config_filter: Box::new(move |path| path.extension() == Some(&ext)),
            ..self
        }
    }

    /// Configures a config dir filter for multiple extensions.
    ///
    /// Sets the config directory filter (see [`config_filter`](#method.config_filter)) to one
    /// matching files with any of the provided extensions.
    pub fn config_exts<I, E>(self, exts: I) -> Self
    where
        I: IntoIterator<Item = E>,
        E: Into<OsString>,
    {
        let exts = exts.into_iter().map(Into::into).collect::<HashSet<_>>();
        Self {
            config_filter: Box::new(move |path| {
                path.extension()
                    .map(|ext| exts.contains(ext))
                    .unwrap_or(false)
            }),
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
    pub fn config_filter<F: FnMut(&Path) -> bool + Send + 'static>(self, filter: F) -> Self {
        Self {
            config_filter: Box::new(filter),
            ..self
        }
    }

    /// Adds another config validator to the chain.
    ///
    /// The validators are there to check, possibly modify and possibly refuse a newly loaded
    /// configuration.
    ///
    /// The callback is passed three parameters:
    ///
    /// * The old configuration.
    /// * The new configuration (possible to modify).
    /// * The command line options.
    ///
    /// They are run in order of being set. Each one can pass arbitrary number of results, where a
    /// result can carry a message (of different severities) that ends up in logs and actions to be
    /// taken if the validation succeeds or fails.
    ///
    /// If none of the validator returns an error-level message, the validation passes. After it is
    /// determined if the configuration passed, either all the success or failure actions are run.
    ///
    /// # The actions
    ///
    /// Sometimes, the only way to validate a piece of config is to try it out ‒ like when you want
    /// to open a listening socket, you don't know if the port is free. But you can't activate and
    /// apply just yet, because something further down the configuration might still fail.
    ///
    /// So, you open the socket (or create an error result) and store it into the success action to
    /// apply it later on. If something fails, the action is dropped and the socket closed.
    ///
    /// The failure action lets you roll back (if it isn't done by simply dropping the thing).
    ///
    /// If the validation and application steps can be separated (you can check if something is OK
    /// by just „looking“ at it ‒ like with a regular expression and using it can't fail), you
    /// don't have to use them, just use verification and [`on_config`](#method.on_config)
    /// separately.
    ///
    /// TODO: Threads, deadlocks
    ///
    /// # Examples
    ///
    /// TODO
    pub fn config_validator<R, F>(self, mut f: F) -> Self
    where
        F: FnMut(&Arc<C>, &mut C, &O) -> R + Send + 'static,
        R: Into<ValidationResults>,
    {
        let wrapper = move |old: &Arc<C>, new: &mut C, opts: &O| f(old, new, opts).into();
        let mut validators = self.config_validators;
        validators.push(Box::new(wrapper));
        Self {
            config_validators: validators,
            ..self
        }
    }

    /// Adds a callback for notification about new configurations.
    ///
    /// The callback is called once a new configuration is loaded and successfully validated.
    ///
    /// TODO: Threads, deadlocks
    pub fn on_config<F: FnMut(&O, &Arc<C>) + Send + 'static>(self, hook: F) -> Self {
        let mut hooks = self.config_hooks;
        hooks.push(Box::new(hook));
        Self {
            config_hooks: hooks,
            ..self
        }
    }

    /// Adds a callback for reacting to a signal.
    ///
    /// The [`Spirit`](struct.Spirit.html) reacts to some signals itself, in its own service
    /// thread. However, it is also possible to hook into any signals directly (well, any except
    /// the ones that are [off limits](https://docs.rs/signal-hook/*/signal_hook/constant.FORBIDDEN.html)).
    ///
    /// These are not run inside the real signal handler, but are delayed and run in the service
    /// thread. Therefore, restrictions about async-signal-safety don't apply to the hook.
    ///
    /// TODO: Threads, deadlocks
    pub fn on_signal<F: FnMut() + Send + 'static>(self, signal: libc::c_int, hook: F) -> Self {
        let mut hooks = self.sig_hooks;
        hooks
            .entry(signal)
            .or_insert_with(Vec::new)
            .push(Box::new(hook));
        Self {
            sig_hooks: hooks,
            ..self
        }
    }

    /// Adds a callback executed once the [`Spirit`](struct.Spirit.html) decides to terminate.
    ///
    /// This is called either when someone calls [`terminate`](struct.Spirit.html#method.terminate)
    /// or when a termination signal is received.
    ///
    /// Note that there are ways the application may terminate without calling these hooks ‒ for
    /// example terminating the main thread, or aborting.
    ///
    /// TODO: Threads, deadlocks
    pub fn on_terminate<F: FnMut() + Send + 'static>(self, hook: F) -> Self {
        let mut hooks = self.terminate_hooks;
        hooks.push(Box::new(hook));
        Self {
            terminate_hooks: hooks,
            ..self
        }
    }

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
    pub fn build(
        mut self,
        background_thread: bool,
    ) -> Result<(Arc<Spirit<O, C>>, InnerBody, WrapBody), Error> {
        debug!("Building the spirit");
        let opts = OptWrapper::<O>::from_args();
        for before_config in &mut self.before_config {
            before_config(&opts.other).context("The before-config phase failed")?;
        }
        let config_files = if opts.common.configs.is_empty() {
            self.config_default_paths
        } else {
            opts.common.configs
        };
        let interesting_signals = self
            .sig_hooks
            .keys()
            .chain(&[libc::SIGHUP, libc::SIGTERM, libc::SIGQUIT, libc::SIGINT])
            .cloned()
            .collect::<HashSet<_>>(); // Eliminate duplicates
        let config = ArcSwap::from(Arc::from(self.config));
        let spirit = Spirit {
            config,
            config_files,
            config_defaults: self.config_defaults,
            config_env: self.config_env,
            config_overrides: opts.common.config_overrides.into_iter().collect(),
            hooks: Mutex::new(Hooks {
                config: self.config_hooks,
                config_filter: self.config_filter,
                config_validators: self.config_validators,
                sigs: self.sig_hooks,
                terminate: self.terminate_hooks,
            }),
            opts: opts.other,
            terminate: AtomicBool::new(false),
        };
        spirit
            .config_reload()
            .context("Problem loading the initial configuration")?;
        let spirit = Arc::new(spirit);
        if background_thread {
            let signals = Signals::new(interesting_signals)?;
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
        Ok((spirit, inner, wrapped))
    }

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
    /// use spirit::{Empty, Spirit};
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
    pub fn run<B: FnOnce(&Arc<Spirit<O, C>>) -> Result<(), Error> + Send + 'static>(self, body: B) {
        let result = utils::log_errors_named("top-level", || {
            let (_spirit, inner, wrapped) = self.before_body(body).build(true)?;
            debug!("Running bodies");
            wrapped.run(inner)
        });
        if result.is_err() {
            process::exit(1);
        }
    }
}
