#![doc(
    html_root_url = "https://docs.rs/spirit/0.1.0/spirit/",
    test(attr(deny(warnings)))
)]
#![cfg_attr(feature = "cargo-clippy", allow(type_complexity))]
// #![deny(missing_docs, warnings)] XXX

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
//!     Spirit::<_, Empty, _>::with_inner_cfg(Cfg::default())
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
//! # Added configuration and options
//!
//! TODO
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
extern crate itertools;
extern crate libc;
#[macro_use]
extern crate log;
extern crate log_panics;
extern crate log_reroute;
extern crate parking_lot;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate signal_hook;
#[macro_use]
extern crate structopt;
extern crate syslog;

mod logging;
pub mod validation;

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt::Debug;
use std::iter;
use std::marker::PhantomData;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::thread;

use arc_swap::{ArcSwap, Lease};
use config::{Config, Environment, File, FileFormat};
use failure::{Error, Fail};
use fallible_iterator::FallibleIterator;
use fern::Dispatch;
use itertools::Itertools;
use log::{LevelFilter, Log};
use parking_lot::Mutex;
use serde::Deserialize;
use signal_hook::iterator::Signals;
use structopt::StructOpt;
use structopt::clap::App;

use logging::{LogDestination, Logging};
use validation::{Level as ValidationLevel, Results as ValidationResults};

#[derive(Deserialize)]
struct ConfigWrapper<C> {
    #[serde(flatten)]
    config: C,
    #[serde(default)]
    logging: Vec<Logging>,
    // TODO: Find a way to detect and report unused fields
}

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
    /// Go into background ‒ daemonize.
    #[structopt(short = "b", long = "background")]
    background: bool,

    /// Override specific config values.
    #[structopt(short = "C", long = "config-override", parse(try_from_str = "key_val"))]
    config_overrides: Vec<(String, String)>,

    /// Configuration files or directories to load.
    #[structopt(parse(from_os_str))]
    configs: Vec<PathBuf>,

    /// Log to stderr with this log level.
    #[structopt(short = "l", long = "log")]
    log: Option<LevelFilter>,

    /// Log to stderr with overriden levels for specific modules.
    #[structopt(short = "L", long = "log-module", parse(try_from_str = "key_val"))]
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

pub fn log_errors<R, F: FnOnce() -> Result<R, Error>>(f: F) -> Result<R, Error> {
    let result = f();
    if let Err(ref e) = result {
        // TODO: Nicer logging with everything
        error!("{}", e);
    }
    result
}

// TODO: Should this be open enum?
#[derive(Debug, Fail)]
#[fail(display = "_1 is not file nor directory")]
struct InvalidFileType(PathBuf);

#[derive(Debug, Deserialize, StructOpt)]
pub struct Empty {}

fn loggers<'a, I: IntoIterator<Item = &'a Logging>>(logging: I) -> Result<(LevelFilter, Box<Log>), Error> {
    let result = logging.into_iter()
        .map(Logging::create)
        .fold_results(Dispatch::new(), Dispatch::chain)?
        .into_log();
    Ok(result)
}

fn install_loggers((max_log_level, top_logger): (LevelFilter, Box<Log>)) {
    log_reroute::reroute_boxed(top_logger);
    log::set_max_level(max_log_level);
}

struct Hooks<O, C> {
    config_filter: Box<Fn(&Path) -> bool + Send>,
    config: Vec<Box<Fn(&Arc<C>) + Send>>,
    config_validators: Vec<Box<Fn(&Arc<C>, &Arc<C>, &O) -> ValidationResults + Send>>,
    sigs: HashMap<libc::c_int, Vec<Box<Fn() + Send>>>,
    terminate: Vec<Box<Fn() + Send>>,
}

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
    extra_logger: Option<Logging>,
    opts: O,
    terminate: AtomicBool,
}

impl<O, C> Spirit<Arc<ArcSwap<C>>, O, C>
where
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: StructOpt,
{
    pub fn with_inner_cfg(cfg: C) -> Builder<Arc<ArcSwap<C>>, O, C> {
        Self::new(Arc::new(ArcSwap::new(Arc::new(cfg))))
    }
}

impl<S, O, C> Spirit<S, O, C>
where
    S: Borrow<ArcSwap<C>> + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync,
    O: StructOpt,
{
    #[cfg_attr(feature = "cargo-clippy", allow(new_ret_no_self))]
    pub fn new(config: S) -> Builder<S, O, C> {
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
        }
    }

    pub fn cmd_opts(&self) -> &O {
        &self.opts
    }

    pub fn config(&self) -> Lease<Arc<C>> {
        self.config.borrow().lease()
    }

    pub fn config_reload(&self) -> Result<(), Error> {
        let config = self.load_config()?;
        // The lock here is across the whole processing, to avoid potential races in logic
        // processing. This makes writing the hooks correctly easier.
        let hooks = self.hooks.lock();
        let new = Arc::new(config.config);
        let old = self.config.borrow().load();
        debug!("Creating new logging");
        // Prepare the logger first, but don't switch until we know we use the new config.
        let loggers = loggers(config.logging.iter().chain(&self.extra_logger))?;
        debug!("Running config validators");
        let mut results = hooks.config_validators
            .iter()
            .map(|v| v(&old, &new, &self.opts))
            .fold(ValidationResults::new(), |mut acc, r| {
                acc.merge(r);
                acc
            });
        for result in &results {
            match result.level() {
                ValidationLevel::Error => {
                    error!(target: "configuration", "{}", result.description());
                },
                ValidationLevel::Warning => {
                    warn!(target: "configuration", "{}", result.description());
                },
                ValidationLevel::Hint => {
                    info!(target: "configuration", "{}", result.description());
                },
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
            return Err(results.into())
        }
        debug!("Validation successful, installing new config");
        for r in &mut results.0 {
            if let Some(success) = r.on_success.as_mut() {
                success();
            }
        }
        debug!("Installing loggers");
        // Once everything is validated, switch to the new logging
        install_loggers(loggers);
        // And to the new config.
        self.config.borrow().store(Arc::clone(&new));
        debug!("Running post-configuration hooks");
        for hook in &hooks.config {
            hook(&new);
        }
        debug!("Configuration reloaded");
        Ok(())
    }

    pub fn is_terminated(&self) -> bool {
        self.terminate.load(Ordering::Relaxed)
    }

    pub fn terminate(&self) {
        debug!("Running termination hooks");
        for hook in &self.hooks.lock().terminate {
            hook();
        }
        self.terminate.store(true, Ordering::Relaxed);
    }

    fn background(&self, signals: &Signals) {
        debug!("Starting background processing");
        for signal in signals.forever() {
            debug!("Received signal {}", signal);
            let term = match signal {
                libc::SIGHUP => {
                    let _ = log_errors(|| self.config_reload());
                    false
                },
                libc::SIGTERM | libc::SIGINT | libc::SIGQUIT => {
                    self.terminate();
                    true
                },
                // Some other signal, only for the hook benefit
                _ => false,
            };

            if let Some(hooks) = self.hooks.lock().sigs.get(&signal) {
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
                let lock = self.hooks.lock();
                let mut files = fallible_iterator::convert(path.read_dir()?)
                    .and_then(|entry| -> Result<Option<PathBuf>, std::io::Error> {
                        let path = entry.path();
                        let meta = path.symlink_metadata()?;
                        if meta.is_file() && (lock.config_filter)(&path) {
                            trace!("Skipping {:?}", path);
                            Ok(Some(path))
                        } else {
                            Ok(None)
                        }
                    })
                    .filter_map(|path| path)
                    .collect::<Vec<_>>()?;
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

pub struct Builder<S, O = Empty, C = Empty> {
    config: S,
    config_default_paths: Vec<PathBuf>,
    config_defaults: Option<(String, FileFormat)>,
    config_env: Option<String>,
    config_hooks: Vec<Box<Fn(&Arc<C>) + Send>>,
    config_filter: Box<Fn(&Path) -> bool + Send>,
    config_validators: Vec<Box<Fn(&Arc<C>, &Arc<C>, &O) -> ValidationResults + Send>>,
    opts: PhantomData<O>,
    sig_hooks: HashMap<libc::c_int, Vec<Box<Fn() + Send>>>,
    terminate_hooks: Vec<Box<Fn() + Send>>,
}

impl<S, O, C> Builder<S, O, C>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
{
    pub fn build(self) -> Result<Arc<Spirit<S, O, C>>, Error> {
        let mut logger = Logging {
            destination: LogDestination::StdErr,
            level: LevelFilter::Warn,
            per_module: HashMap::new(),
        };
        log_reroute::init()?;
        install_loggers(loggers(iter::once(&logger)).unwrap());
        debug!("Building the spirit");
        log_panics::init();
        let opts = OptWrapper::<O>::from_args();
        if let Some(level) = opts.common.log {
            logger.level = level;
            logger.per_module = opts.common.log_modules.iter().cloned().collect();
            install_loggers(loggers(iter::once(&logger))?);
        }
        let config_files = if opts.common.configs.is_empty() {
            self.config_default_paths
        } else {
            opts.common.configs
        };
        let interesting_signals = self.sig_hooks
            .keys()
            .chain(&[libc::SIGHUP, libc::SIGTERM, libc::SIGQUIT, libc::SIGINT])
            .cloned()
            .collect::<HashSet<_>>(); // Eliminate duplicates
        let log_modules = opts.common.log_modules;
        let extra_logger = opts.common
            .log
            .map(|level| {
                Logging {
                    destination: LogDestination::StdErr,
                    level,
                    per_module: log_modules.into_iter().collect(),
                }
            });
        let spirit = Spirit {
            config: self.config,
            config_files,
            config_defaults: self.config_defaults,
            config_env: self.config_env,
            config_overrides: opts.common
                .config_overrides
                .into_iter()
                .collect(),
            extra_logger,
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
            })
            .unwrap(); // Could fail only if the name contained \0
        Ok(spirit)
    }

    pub fn run<E, M>(self, main: M)
    where
        E: Into<Error>,
        M: FnOnce(&Spirit<S, O, C>) -> Result<(), E>,
    {
        let result = log_errors(|| {
            self.build()
                .and_then(|spirit| main(&spirit).map_err(Into::into))
        });
        if result.is_err() {
            process::exit(1);
        }
    }

    pub fn config_default_paths<P, I>(self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        let paths = paths.into_iter()
            .map(Into::into)
            .collect();
        Self {
            config_default_paths: paths,
            .. self
        }
    }

    pub fn config_defaults<D: Into<String>>(self, config: D, format: FileFormat) -> Self {
        Self {
            config_defaults: Some((config.into(), format)),
            .. self
        }
    }

    pub fn config_env<E: Into<String>>(self, env: E) -> Self {
        Self {
            config_env: Some(env.into()),
            .. self
        }
    }

    pub fn config_ext<E: Into<OsString>>(self, ext: E) -> Self {
        let ext = ext.into();
        Self {
            config_filter: Box::new(move |path| path.extension() == Some(&ext)),
            .. self
        }
    }

    pub fn config_exts<I, E>(self, exts: I) -> Self
    where
        I: IntoIterator<Item = E>,
        E: Into<OsString>,
    {
        let exts = exts.into_iter()
            .map(Into::into)
            .collect::<HashSet<_>>();
        Self {
            config_filter: Box::new(move |path| {
                path.extension()
                    .map(|ext| exts.contains(ext))
                    .unwrap_or(false)
            }),
            .. self
        }
    }

    pub fn config_filter<F: Fn(&Path) -> bool + Send + 'static>(self, filter: F) -> Self {
        Self {
            config_filter: Box::new(filter),
            .. self
        }
    }

    pub fn config_validator<R, F>(self, f: F) -> Self
    where
        F: Fn(&Arc<C>, &Arc<C>, &O) -> R + Send + 'static,
        R: Into<ValidationResults>,
    {
        let wrapper = move |old: &Arc<C>, new: &Arc<C>, opts: &O| f(old, new, opts).into();
        let mut validators = self.config_validators;
        validators.push(Box::new(wrapper));
        Self {
            config_validators: validators,
            .. self
        }
    }

    pub fn on_config<F: Fn(&Arc<C>) + Send + 'static>(self, hook: F) -> Self {
        let mut hooks = self.config_hooks;
        hooks.push(Box::new(hook));
        Self {
            config_hooks: hooks,
            .. self
        }
    }

    pub fn on_signal<F: Fn() + Send + 'static>(self, signal: libc::c_int, hook: F) -> Self {
        let mut hooks = self.sig_hooks;
        hooks.entry(signal)
            .or_insert_with(Vec::new)
            .push(Box::new(hook));
        Self {
            sig_hooks: hooks,
            .. self
        }
    }

    pub fn on_terminate<F: Fn() + Send + 'static>(self, hook: F) -> Self {
        let mut hooks = self.terminate_hooks;
        hooks.push(Box::new(hook));
        Self {
            terminate_hooks: hooks,
            .. self
        }
    }
}

// TODO: Provide contexts for thisg
// TODO: Sort the directory when traversing
