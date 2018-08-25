#![doc(
    html_root_url = "https://docs.rs/spirit/0.1.0/spirit/",
    test(attr(deny(warnings)))
)]
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
//! This crate is supposed to help with the former. Before using, you should know the following:
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
//! # What the crate does and how
//!
//! To be honest, the crate doesn't bring much (or, maybe mostly none) of novelty functionality to
//! the table. It just takes other crates doing something useful and gluing them together to form
//! something most daemons want to do.
//!
//! Using the builder pattern, you create a singleton [`Spirit`] object. That one starts a
//! background thread that runs some callbacks configured previous when things happen.
//!
//! It takes two structs, one for command line arguments (using [StructOpt]) and another for
//! configuration (implementing [serde]'s [`Deserialize`], loaded using the [config] crate). It
//! enriches both to add common options, like configuration overrides on the command line and
//! logging into the configuration.
//!
//! The background thread listens to certain signals (like `SIGHUP`) using the [signal-hook] crate
//! and reloads the configuration when requested. It manages the logging backend to reopen on
//! `SIGHUP` and reflect changes to the configuration.
//!
//! [`Spirit`]: struct.Spirit.html
//! [StructOpt]: https://crates.io/crates/structopt
//! [serde]: https://crates.io/crates/serde
//! [`Deserialize`]: https://docs.rs/serde/*/serde/trait.Deserialize.html
//! [config]: https://crates.io/crates/config
//! [signal-hook]: https://crates.io/crates/signal-hook
//!
//! # Examples
//!
//! ```rust
//! extern crate config;
//! #[macro_use]
//! extern crate log;
//! #[macro_use]
//! extern crate serde_derive;
//! extern crate spirit;
//!
//! use std::io::Error;
//! use std::time::Duration;
//! use std::thread;
//!
//! use config::FileFormat;
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
//!     Spirit::<_, Empty, _>::new(Cfg::default())
//!         // Provide default values for the configuration
//!         .config_defaults(DEFAULT_CFG, FileFormat::Toml)
//!         // If the program is passed a directory, load files with these extensions from there
//!         .config_exts(&["toml", "ini", "json"])
//!         .on_terminate(|| debug!("Asked to terminate"))
//!         .on_config(|cfg| debug!("New config loaded: {:?}", cfg))
//!         // Run the closure, logging the error nicely if it happens (note: no error happens
//!         // here)
//!         .run(|spirit| -> Result<(), Error> {
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
//! * `debug`: When this is set, the program doesn't become a daemon and stays in the foreground.
//!   It preserves the stdio.
//! * `config-override`: Override configuration value.
//! * `log`: In addition to the logging in configuration file, also log with the given severity to
//!   stderr.
//! * `log-module`: Override the stderr log level of the given module.
//!
//! Furthermore, it takes a list of paths ‒ both files and directories. They are loaded as
//! configuration files (the directories are examined and files in them ‒ the ones passing a
//! [filter](struct.Builder.html#method.config_files) ‒ are also loaded).
//!
//! ```sh
//! ./program --debug --log info --log-module program=trace --config-override ui.message=something
//! ```
//!
//! ## Configuration options
//!
//! ### `logging`
//!
//! It is an array of logging destinations. No matter where the logging is sent to, these options
//! are valid for all:
//!
//! * `level`: The log level to use. Valid options are `OFF`, `ERROR`, `WARN`, `INFO`, `DEBUG` and
//!   `TRACE`.
//! * `per-module`: A map, setting log level overrides for specific modules (logging targets). This
//!   one is optional.
//! * `type`: Specifies the type of logger destination. Some of them allow specifying other
//!   options.
//!
//! The allowed types are:
//! * `stdout`: The logs are sent to standard output. There are no additional options.
//! * `stderr`: The logs are sent to standard error output. There are no additional options.
//! * `file`: Logs are written to a file. The file is reopened every time a configuration is
//!   re-read (therefore every time the application gets `SIGHUP`), which makes it work with
//!   logrotate.
//!   - `filename`: The path to the file where to put the logs.
//! * `network`: The application connects to a given host and port over TCP and sends logs there.
//!   - `host`: The hostname (or IP address) to connect to.
//!   - `port`: The port to use.
//! * `syslog`: Sends the logs to syslog.
//!
//! ### `daemon`
//!
//! Influences how daemonization is done.
//!
//! * `user`: The user to become. Either a numeric ID or name. If not present, it doesn't change
//!   the user.
//! * `group`: Similar as user, but with group.
//! * `pid_file`: A pid file to write on startup. If not present, nothing is stored.
//! * `workdir`: A working directory it'll switch into. If not set, defaults to `/`.
//!
//! # Multithreaded applications
//!
//! As daemonization is done by using `fork`, you should start any threads *after* you initialize
//! the `spirit`. Otherwise you'll lose the threads (and further bad things will happen).
//!
//! # Common patterns
//!
//! TODO

extern crate arc_swap;
extern crate chrono;
extern crate config;
#[macro_use]
extern crate failure;
extern crate fallible_iterator;
extern crate fern;
#[cfg(feature = "tokio-helpers")]
extern crate futures;
extern crate itertools;
extern crate libc;
#[macro_use]
extern crate log;
extern crate log_panics;
extern crate log_reroute;
extern crate nix;
extern crate parking_lot;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate signal_hook;
// For some reason, this produces a warning about unused on nightly… but it is needed on stable
#[allow(unused_imports)]
#[macro_use]
extern crate structopt;
extern crate syslog;
#[cfg(feature = "tokio-helpers")]
extern crate tokio;

pub mod helpers;
mod logging;
pub mod validation;

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::OsString;
use std::fmt::Debug;
use std::fs::OpenOptions;
use std::io::Write;
use std::iter;
use std::marker::PhantomData;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use arc_swap::{ArcSwap, Lease};
use config::{Config, Environment, File, FileFormat};
use failure::{Error, Fail};
use fallible_iterator::FallibleIterator;
use log::LevelFilter;
use nix::sys::stat::{self, Mode};
use nix::unistd::{self, ForkResult, Gid, Uid};
use parking_lot::Mutex;
use serde::Deserialize;
use signal_hook::iterator::Signals;
use structopt::clap::App;
use structopt::StructOpt;

use helpers::tokio::{TokioGuts, TokioGutsInner};
use helpers::Helper;
use logging::{LogDestination, Logging};
use validation::{
    Error as ValidationError, Level as ValidationLevel, Results as ValidationResults,
};

pub use logging::SyslogError;

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
enum SecId {
    Name(String),
    Id(u32),
    #[serde(skip)]
    Nothing,
}

impl Default for SecId {
    fn default() -> Self {
        SecId::Nothing
    }
}

#[derive(Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
struct Daemon {
    #[serde(default)]
    user: SecId,
    #[serde(default)]
    group: SecId,
    pid_file: Option<PathBuf>,
    workdir: Option<PathBuf>,
}

#[derive(Deserialize)]
struct ConfigWrapper<C> {
    #[serde(flatten)]
    config: C,
    #[serde(default)]
    daemon: Daemon,
    #[serde(default)]
    logging: Vec<Logging>,
    // TODO: Find a way to detect and report unused fields
}

/// An error returned when the user passes a key-value option without equal sign.
///
/// Some internal options take a key-value pairs on the command line. If such option is expected,
/// but it doesn't contain the equal sign, this is the used error.
#[derive(Debug, Fail)]
#[fail(display = "Missing = in map option")]
pub struct MissingEquals;

// TODO: This one probably needs tests ‒ +- errors, parsing of both allowed and disallowed things,
// missing equals…
fn key_val<K, V>(opt: &str) -> Result<(K, V), Error>
where
    K: FromStr,
    K::Err: Fail + 'static,
    V: FromStr,
    V::Err: Fail + 'static,
{
    let pos = opt.find('=').ok_or(MissingEquals)?;
    Ok((opt[..pos].parse()?, opt[pos + 1..].parse()?))
}

#[derive(Debug, StructOpt)]
struct CommonOpts {
    /// Override specific config values.
    #[structopt(
        short = "C",
        long = "config-override",
        parse(try_from_str = "key_val"),
        raw(number_of_values = "1"),
    )]
    config_overrides: Vec<(String, String)>,

    /// Configuration files or directories to load.
    #[structopt(parse(from_os_str))]
    configs: Vec<PathBuf>,

    /// Debug support - Don't go into background (daemonize) and don't redirect stdio.
    #[structopt(short = "d", long = "debug")]
    debug: bool,

    /// Log to stderr with this log level.
    #[structopt(short = "l", long = "log", raw(number_of_values = "1"))]
    log: Option<LevelFilter>,

    /// Log to stderr with overriden levels for specific modules.
    #[structopt(
        short = "L",
        long = "log-module",
        parse(try_from_str = "key_val"),
        raw(number_of_values = "1"),
    )]
    log_modules: Vec<(String, LevelFilter)>,
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

/// A wrapper around a fallible function that logs any returned errors, with all the causes and
/// optionally the backtrace.
pub fn log_errors<R, F: FnOnce() -> Result<R, Error>>(f: F) -> Result<R, Error> {
    let result = f();
    if let Err(ref e) = result {
        // Note: one of the causes is the error itself
        for cause in e.iter_chain() {
            error!("{}", cause);
        }
        let bt = format!("{}", e.backtrace());
        if !bt.is_empty() {
            debug!("{}", bt);
        }
    }
    result
}

/// An error returned whenever the user passes something not a file nor a directory as
/// configuration.
#[derive(Debug, Fail)]
#[fail(display = "_1 is not file nor directory")]
pub struct InvalidFileType(PathBuf);

/// A struct that may be used when either configuration or command line options are not needed.
///
/// When the application doesn't need the configuration (in excess of the automatic part provided
/// by this library) or it doesn't need any command line options of its own, this struct can be
/// used to plug the type parameter.
#[derive(Debug, Deserialize, StructOpt)]
pub struct Empty {}

struct Hooks<O, C> {
    config_filter: Box<FnMut(&Path) -> bool + Send>,
    config: Vec<Box<FnMut(&Arc<C>) + Send>>,
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
/// Spirit::<_, Empty, _>::new(Empty {})
///     .on_config(|_new_cfg| {
///         // Adapt to new config here
///     })
///     .run(|_spirit| -> Result<(), std::io::Error> {
///         // Application runs here
///         Ok(())
///     });
/// ```
pub struct Spirit<S, O = Empty, C = Empty>
where
    S: Borrow<ArcSwap<C>> + 'static,
{
    config: S,
    hooks: Mutex<Hooks<O, C>>,
    // TODO: Mode selection for directories
    config_files: Vec<PathBuf>,
    config_defaults: Option<(String, FileFormat)>,
    config_env: Option<String>,
    config_overrides: HashMap<String, String>,
    debug: bool,
    extra_logger: Option<Logging>,
    opts: O,
    previous_daemon: Mutex<Option<Daemon>>,
    terminate: AtomicBool,

    // Either empty or top-level futures to spawn on tokio-run. It is unused in this module.
    #[allow(dead_code)]
    tokio_guts: TokioGuts<Arc<Spirit<S, O, C>>>,
}

/// A type alias for a `Spirit` with configuration held inside.
///
/// Just to ease up naming the type when passing around the program.
pub type SpiritInner<O, C> = Arc<Spirit<Arc<ArcSwap<C>>, O, C>>;

impl<O, C> Spirit<Arc<ArcSwap<C>>, O, C>
where
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: StructOpt,
{
    /// A constructor for spirit with only inner-accessible configuration.
    ///
    /// With this constructor, you can look into the current configuration by calling the
    /// [`config'](#method.config), but it doesn't store it into a globally accessible storage.
    ///
    /// # Examples
    ///
    /// ```
    /// use spirit::{Empty, Spirit};
    ///
    /// Spirit::<_, Empty, _>::new(Empty {})
    ///     .run(|spirit| -> Result<(), std::io::Error> {
    ///         println!("The config is: {:?}", spirit.config());
    ///         Ok(())
    ///     });
    /// ```
    #[cfg_attr(feature = "cargo-clippy", allow(new_ret_no_self))]
    pub fn new(cfg: C) -> Builder<Arc<ArcSwap<C>>, O, C> {
        Self::with_config_storage(Arc::new(ArcSwap::new(Arc::new(cfg))))
    }
}

impl<S, O, C> Spirit<S, O, C>
where
    S: Borrow<ArcSwap<C>> + Send + Sync + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync,
    O: StructOpt,
{
    /// A constructor with specifying external config storage.
    ///
    /// This way the spirit keeps the current config in the provided storage, so it is accessible
    /// from outside as well as through the [`config`](#method.config) method.
    pub fn with_config_storage(config: S) -> Builder<S, O, C> {
        Builder {
            config,
            config_default_paths: Vec::new(),
            config_defaults: None,
            config_env: None,
            config_hooks: Vec::new(),
            config_filter: Box::new(|_| false),
            config_validators: Vec::new(),
            opts: PhantomData,
            sig_hooks: HashMap::new(),
            terminate_hooks: Vec::new(),
            tokio_guts: TokioGutsInner::default(),
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
    /// let spirit = Spirit::<_, Empty, _>::new(Empty {})
    ///     .build()
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
        self.config.borrow().lease()
    }

    fn daemonize(&self, daemon: &Daemon) -> Result<(), Error> {
        // TODO: Discovering by name
        // TODO: Tests for this
        debug!("Preparing to daemonize with {:?}", daemon);
        stat::umask(Mode::empty()); // No restrictions on write modes
        let workdir = daemon
            .workdir
            .as_ref()
            .map(|pb| pb as &Path)
            .unwrap_or_else(|| Path::new("/"));
        trace!("Changing working directory to {:?}", workdir);
        env::set_current_dir(workdir)?;
        if !self.debug {
            trace!("Redirecting stdio");
            let devnull = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open("/dev/null")?;
            for fd in &[0, 1, 2] {
                unistd::dup2(devnull.as_raw_fd(), *fd)?;
            }
            trace!("Doing double fork");
            if let ForkResult::Parent { .. } = unistd::fork()? {
                process::exit(0);
            }
            unistd::setsid()?;
        }
        if let ForkResult::Parent { .. } = unistd::fork()? {
            process::exit(0);
        }
        if let Some(ref file) = daemon.pid_file {
            let mut f = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o644)
                .open(file)?;
            writeln!(f, "{}", unistd::getpid())?;
        }
        match daemon.group {
            SecId::Id(id) => unistd::setgid(Gid::from_raw(id))?,
            SecId::Name(_) => unimplemented!("Discovering group name"),
            SecId::Nothing => (),
        }
        match daemon.user {
            SecId::Id(id) => unistd::setuid(Uid::from_raw(id))?,
            SecId::Name(_) => unimplemented!("Discovering user name"),
            SecId::Nothing => (),
        }
        Ok(())
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
        let config = self.load_config()?;
        // The lock here is across the whole processing, to avoid potential races in logic
        // processing. This makes writing the hooks correctly easier.
        let mut hooks = self.hooks.lock();
        let old = self.config.borrow().load();
        let mut new = config.config;
        debug!("Creating new logging");
        // Prepare the logger first, but don't switch until we know we use the new config.
        let loggers = logging::create(config.logging.iter().chain(&self.extra_logger))?;
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
                    error!(target: "configuration", "{}", result.description());
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
            return Err(ValidationError.into());
        }
        debug!("Validation successful, installing new config");
        for r in &mut results.0 {
            if let Some(success) = r.on_success.as_mut() {
                success();
            }
        }
        debug!("Installing loggers");
        // Once everything is validated, switch to the new logging
        logging::install(loggers);
        // And to the new config.
        self.config.borrow().store(Arc::clone(&new));
        debug!("Running post-configuration hooks");
        for hook in &mut hooks.config {
            hook(&new);
        }
        debug!("Configuration reloaded");
        let mut daemon = self.previous_daemon.lock();
        if let Some(ref daemon) = *daemon {
            if daemon != &config.daemon {
                warn!("Can't change daemon configuration at runtime");
            }
        } else {
            self.daemonize(&config.daemon)?;
        }
        *daemon = Some(config.daemon);
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
    /// let spirit = Spirit::<_, Empty, _>::new(Empty {})
    ///     .build()
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
                    let _ = log_errors(|| self.config_reload());
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

    fn load_config(&self) -> Result<ConfigWrapper<C>, Error> {
        debug!("Loading configuration");
        let mut config = Config::new();
        // To avoid problems with trying to parse without any configuration present (it would
        // complain that it found unit and whatever the config was is expected instead).
        config.merge(File::from_str("", FileFormat::Toml))?;
        if let Some((ref defaults, format)) = self.config_defaults {
            trace!("Loading config defaults");
            config.merge(File::from_str(defaults, format))?;
        }
        for path in &self.config_files {
            if path.is_file() {
                trace!("Loading config file {:?}", path);
                config.merge(File::from(path as &Path))?;
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
                            trace!("Skipping {:?}", path);
                            Ok(Some(path))
                        } else {
                            Ok(None)
                        }
                    }).filter_map(|path| path)
                    .collect::<Vec<_>>()?;
                // Traverse them sorted.
                files.sort();
                for file in files {
                    trace!("Loading config file {:?}", file);
                    config.merge(File::from(file))?;
                }
            } else {
                bail!(InvalidFileType(path.to_owned()));
            }
        }
        if let Some(env_prefix) = self.config_env.as_ref() {
            trace!("Loading config from environment {}", env_prefix);
            config.merge(Environment::with_prefix(env_prefix))?;
        }
        for (ref key, ref value) in &self.config_overrides {
            trace!("Config override {} => {}", key, value);
            config.set(*key, *value as &str)?;
        }
        Ok(config.try_into()?)
    }
}

/// The builder of [`Spirit`](struct.Spirit.html).
///
/// This is returned by the [`Spirit::new`](struct.Spirit.html#new).
pub struct Builder<S, O = Empty, C = Empty>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
{
    config: S,
    config_default_paths: Vec<PathBuf>,
    config_defaults: Option<(String, FileFormat)>,
    config_env: Option<String>,
    config_hooks: Vec<Box<FnMut(&Arc<C>) + Send>>,
    config_filter: Box<FnMut(&Path) -> bool + Send>,
    config_validators: Vec<Box<FnMut(&Arc<C>, &mut C, &O) -> ValidationResults + Send>>,
    opts: PhantomData<O>,
    sig_hooks: HashMap<libc::c_int, Vec<Box<FnMut() + Send>>>,
    terminate_hooks: Vec<Box<FnMut() + Send>>,
    tokio_guts: TokioGutsInner<Arc<Spirit<S, O, C>>>,
}

impl<S, O, C> Builder<S, O, C>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
{
    /// Finish building the Spirit.
    ///
    /// This transitions from the configuration phase of Spirit to actually creating it and
    /// launching it.
    ///
    /// This starts listening for signals, loads the configuration for the first time and starts
    /// the background thread.
    ///
    /// This version returns the spirit (or error) and error handling is up to the caller. If you
    /// want spirit to take care of nice error logging (even for your application's top level
    /// errors), use [`run`](#method.run).
    ///
    /// # Warning
    ///
    /// Unless in debug mode, this forks. You want to run this before you start any threads, or
    /// you'll lose them.
    pub fn build(self) -> Result<Arc<Spirit<S, O, C>>, Error> {
        let mut logger = Logging {
            destination: LogDestination::StdErr,
            level: LevelFilter::Warn,
            per_module: HashMap::new(),
        };
        log_reroute::init()?;
        logging::install(logging::create(iter::once(&logger)).unwrap());
        debug!("Building the spirit");
        log_panics::init();
        let opts = OptWrapper::<O>::from_args();
        if let Some(level) = opts.common.log {
            logger.level = level;
            logger.per_module = opts.common.log_modules.iter().cloned().collect();
            logging::install(logging::create(iter::once(&logger))?);
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
        let log_modules = opts.common.log_modules;
        let extra_logger = opts.common.log.map(|level| Logging {
            destination: LogDestination::StdErr,
            level,
            per_module: log_modules.into_iter().collect(),
        });
        let spirit = Spirit {
            config: self.config,
            config_files,
            config_defaults: self.config_defaults,
            config_env: self.config_env,
            config_overrides: opts.common.config_overrides.into_iter().collect(),
            debug: opts.common.debug,
            extra_logger,
            hooks: Mutex::new(Hooks {
                config: self.config_hooks,
                config_filter: self.config_filter,
                config_validators: self.config_validators,
                sigs: self.sig_hooks,
                terminate: self.terminate_hooks,
            }),
            opts: opts.other,
            previous_daemon: Mutex::new(None),
            terminate: AtomicBool::new(false),
            tokio_guts: self.tokio_guts.into(),
        };
        spirit.config_reload()?;
        let signals = Signals::new(interesting_signals)?;
        let spirit = Arc::new(spirit);
        let spirit_bg = Arc::clone(&spirit);
        thread::Builder::new()
            .name("spirit".to_owned())
            .spawn(move || {
                loop {
                    // Note: we run a bunch of callbacks inside the service thread. We restart the
                    // thread if it fails.
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
            }).unwrap(); // Could fail only if the name contained \0
        Ok(spirit)
    }

    /// Build the spirit and run the application, handling all relevant errors.
    ///
    /// In case an error happens (either when creating the Spirit, or returned by the callback),
    /// the errors are logged (either to the place where logs are sent to in configuration, or to
    /// stderr if the error happens before logging is initialized ‒ for example if configuration
    /// can't be read). The application then terminates with failure exit code.
    ///
    /// ```rust
    /// use std::thread;
    /// use std::time::Duration;
    ///
    /// use spirit::{Empty, Spirit};
    ///
    /// Spirit::<_, Empty, _>::new(Empty {})
    ///     .run(|spirit| -> Result<(), std::io::Error> {
    ///         while !spirit.is_terminated() {
    ///             // Some reasonable work here
    ///             thread::sleep(Duration::from_millis(100));
    /// #           spirit.terminate();
    ///         }
    ///
    ///         Ok(())
    ///     });
    /// ```
    pub fn run<E, M>(self, main: M)
    where
        E: Into<Error>,
        M: FnOnce(Arc<Spirit<S, O, C>>) -> Result<(), E>,
    {
        let result = log_errors(|| {
            self.build()
                .and_then(|spirit| main(spirit).map_err(Into::into))
        });
        if result.is_err() {
            process::exit(1);
        }
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
    /// This „loads“ the lowest layer of the configuration from the passed string.
    pub fn config_defaults<D: Into<String>>(self, config: D, format: FileFormat) -> Self {
        Self {
            config_defaults: Some((config.into(), format)),
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
    /// extern crate config;
    /// extern crate failure;
    /// #[macro_use]
    /// extern crate serde_derive;
    /// extern crate spirit;
    ///
    /// use config::FileFormat;
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
    ///     Spirit::<_, Empty, _>::new(Cfg::default())
    ///         .config_defaults(DEFAULT_CFG, FileFormat::Toml)
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

    pub fn helper<H: Helper<S, O, C>>(self, helper: H) -> Self {
        helper.apply(self)
    }

    /// Adds a callback for notification about new configurations.
    ///
    /// The callback is called once a new configuration is loaded and successfully validated.
    ///
    /// TODO: Threads, deadlocks
    pub fn on_config<F: FnMut(&Arc<C>) + Send + 'static>(self, hook: F) -> Self {
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
}
