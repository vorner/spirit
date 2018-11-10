#![doc(
    html_root_url = "https://docs.rs/spirit-log/0.1.3/spirit_log/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! A spirit configuration helper for logging.
//!
//! Set of configuration fragments and config helpers for the
//! [`spirit`](https://crates.io/crates/spirit) configuration framework that configures logging for
//! the [`log`](https://crates.io/crates/log) crate and updates it as the configuration changes
//! (even at runtime).
//!
//! Currently, it also allows asking for log output on the command line and multiple logging
//! destinations with different log levels set.
//!
//! It assumes the application doesn't set the global logger (this crate sets it on its own).
//!
//! For details about added options, see the [`Opts`](struct.Opts.html) and
//! [`Cfg`](struct.Cfg.html) configuration fragments.
//!
//! # Startup
//!
//! The logging is set in multiple steps:
//!
//! * As soon as the config helper is registered, a logging on the `WARN` level is sent to
//!   `stderr`.
//! * After command line arguments are parsed the `stderr` logging is updated to reflect that (or
//!   left on the `WARN` level if nothing is set by the user).
//! * After configuration is loaded from the files, full logging is configured.
//!
//! # Integration with other loggers
//!
//! If you need something specific (for example [`sentry`](https://crates.io/crates/sentry)), you
//! can provide functions to add additional loggers in parallel with the ones created in this
//! crate. The crate will integrate them all together.
//!
//! # Performance warning
//!
//! This allows the user to create arbitrary number of loggers. Furthermore, the logging is
//! synchronous and not buffered. When writing a lot of logs or sending them over the network, this
//! could become a bottleneck.
//!
//! # Planned features
//!
//! These pieces are planned some time in future, but haven't happened yet.
//!
//! * Reconnecting to the remote server if a TCP connection is lost.
//! * Log file rotation.
//! * Setting of logging format (choosing either local or UTC, the format argument or JSON).
//! * Colors on `stdout`/`stderr`.
//! * Async and buffered logging and ability to drop log messages when logging doesn't keep up.
//!
//! # Examples
//!
//! ```rust
//! #[macro_use]
//! extern crate log;
//! extern crate spirit;
//! extern crate spirit_log;
//! #[macro_use]
//! extern crate serde_derive;
//! #[macro_use]
//! extern crate structopt;
//!
//! use spirit::Spirit;
//! use spirit_log::{Cfg as LogCfg, Opts as LogOpts};
//!
//! #[derive(Clone, Debug, StructOpt)]
//! struct Opts {
//!     #[structopt(flatten)]
//!     log: LogOpts,
//! }
//!
//! impl Opts {
//!     fn log(&self) -> LogOpts {
//!         self.log.clone()
//!     }
//! }
//!
//! #[derive(Clone, Debug, Default, Deserialize)]
//! struct Cfg {
//!     #[serde(flatten)]
//!     log: LogCfg,
//! }
//!
//! impl Cfg {
//!     fn log(&self) -> LogCfg {
//!         self.log.clone()
//!     }
//! }
//!
//! fn main() {
//!     Spirit::<Opts, Cfg>::new()
//!         .config_helper(Cfg::log, Opts::log, "logging")
//!         .run(|_spirit| {
//!             info!("Hello world");
//!             Ok(())
//!         });
//! }
//! ```
//!
//! The configuration could look something like this:
//!
//! ```toml
//! [[logging]]
//! level = "DEBUG"
//! type = "file"
//! filename = "/tmp/example.log"
//! clock = "UTC"
//! ```

extern crate chrono;
#[allow(unused_imports)]
#[macro_use]
extern crate failure;
extern crate fern;
extern crate itertools;
#[macro_use]
extern crate log;
extern crate log_reroute;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
extern crate spirit;
#[allow(unused_imports)]
#[macro_use]
extern crate structopt;
extern crate syslog;

use std::cmp;
use std::collections::HashMap;
use std::fmt::{Arguments, Debug, Display};
use std::io::{self, Write};
use std::iter;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use chrono::format::{DelayedFormat, StrftimeItems};
use chrono::{Local, Utc};
use failure::{Error, Fail};
use fern::Dispatch;
use itertools::Itertools;
use log::{LevelFilter, Log, Metadata, Record};
use serde::de::{Deserialize, DeserializeOwned, Deserializer, Error as DeError};
use spirit::helpers::CfgHelper;
use spirit::validation::{Result as ValidationResult, Results as ValidationResults};
use spirit::Builder;
use structopt::StructOpt;

struct MultiLog(Vec<Box<Log>>);

impl Log for MultiLog {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.0.iter().any(|l| l.enabled(metadata))
    }
    fn log(&self, record: &Record) {
        for sub in &self.0 {
            sub.log(record)
        }
    }
    fn flush(&self) {
        for sub in &self.0 {
            sub.flush()
        }
    }
}

/// A fragment for command line options.
///
/// By flattening this into the top-level `StructOpt` structure, you get the `-l` and `-L` command
/// line options. The `-l` (`--log`) sets the global logging level for `stderr`. The `-L` accepts
/// pairs (eg. `-L spirit=TRACE`) specifying levels for specific logging targets.
#[derive(Clone, Debug, StructOpt)]
pub struct Opts {
    /// Log to stderr with this log level.
    #[structopt(short = "l", long = "log", raw(number_of_values = "1"))]
    log: Option<LevelFilter>,

    /// Log to stderr with overriden levels for specific modules.
    #[structopt(
        short = "L",
        long = "log-module",
        parse(try_from_str = "spirit::key_val"),
        raw(number_of_values = "1")
    )]
    log_modules: Vec<(String, LevelFilter)>,
}

impl Opts {
    fn logger_cfg(&self) -> Option<Logger> {
        self.log.map(|level| Logger {
            level,
            destination: LogDestination::StdErr,
            per_module: self.log_modules.iter().cloned().collect(),
            clock: Clock::Local,
            time_format: cmdline_time_format(),
            format: Format::Short,
        })
    }
}

// TODO: OptsExt & OptsVerbose and turn the other things into Into<Opts>

type ExtraLogger<O, C> =
    Box<Fn(&O, &C) -> Result<(LevelFilter, Vec<Box<Log>>), Error> + Send + Sync>;

/// Description of extra configuration for logging.
///
/// This allows customizing the logging as managed by this crate, mostly by pairing it with command
/// line options (see [`Opts`](struct.Opts.html)) and adding arbitrary additional loggers.
///
/// The default does nothing (eg. no command line options and no extra loggers).
pub struct Extras<O, C> {
    opts: Option<Box<Fn(&O) -> Opts + Send + Sync>>,
    loggers: Vec<ExtraLogger<O, C>>,
}

impl<O, C> Default for Extras<O, C> {
    fn default() -> Self {
        Extras {
            opts: None,
            loggers: Vec::new(),
        }
    }
}

impl<O, C> Extras<O, C> {
    /// Constructs the `Extras` structure with an extractor for options.
    ///
    /// The passed closure should take the applications global options structure and return the
    /// `Opts` by which the logging from command line can be tweaked.
    pub fn with_opts<F: Fn(&O) -> Opts + Send + Sync + 'static>(extractor: F) -> Self {
        Extras {
            opts: Some(Box::new(extractor)),
            loggers: Vec::new(),
        }
    }

    /// Modifies the `Extras` to contain another source of additional loggers.
    ///
    /// Each such closure is run every time logging is being configured. It can return either bunch
    /// of loggers (with the needed log level), or an error. If an error is returned, the loading
    /// of new configuration is aborted.
    pub fn extra_loggers<F>(self, extra: F) -> Self
    where
        F: Fn(&O, &C) -> Result<(LevelFilter, Vec<Box<Log>>), Error> + Send + Sync + 'static,
    {
        let mut loggers = self.loggers;
        loggers.push(Box::new(extra));
        Extras { loggers, ..self }
    }
    fn opts(&self, opts: &O) -> Option<Logger> {
        self.opts.as_ref().and_then(|ext| ext(opts).logger_cfg())
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")] // TODO: Make deny-unknown-fields work
enum LogDestination {
    File {
        filename: PathBuf,
        // TODO: Truncate
    },
    Syslog {
        host: Option<String>,
        // TODO: Remote syslog
    },
    Network {
        host: String,
        port: u16,
    },
    #[serde(rename = "stdout")]
    StdOut, // TODO: Colors
    #[serde(rename = "stderr")]
    StdErr, // TODO: Colors
}

fn default_level_filter() -> LevelFilter {
    LevelFilter::Error
}

fn deserialize_level_filter<'de, D: Deserializer<'de>>(d: D) -> Result<LevelFilter, D::Error> {
    let s = String::deserialize(d)?;
    s.parse().map_err(|_| {
        D::Error::unknown_variant(&s, &["OFF", "ERROR", "WARN", "INFO", "DEBUG", "TRACE"])
    })
}

fn deserialize_per_module<'de, D>(d: D) -> Result<HashMap<String, LevelFilter>, D::Error>
where
    D: Deserializer<'de>,
{
    HashMap::<String, String>::deserialize(d)?
        .into_iter()
        .map(|(k, v)| {
            let parsed = v.parse().map_err(|_| {
                D::Error::unknown_variant(&v, &["OFF", "ERROR", "WARN", "INFO", "DEBUG", "TRACE"])
            })?;
            Ok((k, parsed))
        })
        .collect()
}

/// This error can be returned when initialization of logging to syslog fails.
#[derive(Debug, Fail)]
#[fail(display = "{}", _0)]
pub struct SyslogError(String);

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum Clock {
    Local,
    Utc,
}

impl Clock {
    fn now(self, format: &str) -> DelayedFormat<StrftimeItems> {
        match self {
            Clock::Local => Local::now().format(format),
            Clock::Utc => Utc::now().format(format),
        }
    }
}

impl Default for Clock {
    fn default() -> Self {
        Clock::Local
    }
}

fn default_time_format() -> String {
    "%+".to_owned()
}

fn cmdline_time_format() -> String {
    "%F %T%.3f".to_owned()
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Format {
    MessageOnly,
    Short,
    Extended,
    Full,
    Machine,
    // TODO: Configurable field names?
    Json,
    Logstash,
    // TODO: Custom
}

impl Default for Format {
    fn default() -> Self {
        Format::Short
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")] // TODO: Make deny-unknown-fields work
struct Logger {
    #[serde(flatten)]
    destination: LogDestination,
    #[serde(
        default = "default_level_filter",
        deserialize_with = "deserialize_level_filter"
    )]
    level: LevelFilter,
    #[serde(default, deserialize_with = "deserialize_per_module")]
    per_module: HashMap<String, LevelFilter>,
    #[serde(default)]
    clock: Clock,
    #[serde(default = "default_time_format")]
    time_format: String,
    #[serde(default)]
    format: Format,
}

impl Logger {
    fn create(&self) -> Result<Dispatch, Error> {
        trace!("Creating logger for {:?}", self);
        let mut logger = Dispatch::new().level(self.level);
        logger = self
            .per_module
            .iter()
            .fold(logger, |logger, (module, level)| {
                logger.level_for(module.clone(), *level)
            });
        let clock = self.clock;
        let time_format = self.time_format.clone();
        let format = self.format;
        match self.destination {
            // We don't want to format syslog
            LogDestination::Syslog { .. } => (),
            // We do with the other things
            _ => {
                logger = logger.format(move |out, message, record| {
                    match format {
                        Format::MessageOnly => out.finish(format_args!("{}", message)),
                        Format::Short => out.finish(format_args!(
                            "{} {:5} {:30} {}",
                            clock.now(&time_format),
                            record.level(),
                            record.target(),
                            message,
                        )),
                        Format::Extended => {
                            let thread = thread::current();
                            out.finish(format_args!(
                                "{} {:5} {:30} {:30} {}",
                                clock.now(&time_format),
                                record.level(),
                                thread.name().unwrap_or("<unknown>"),
                                record.target(),
                                message,
                            ));
                        }
                        Format::Full => {
                            let thread = thread::current();
                            out.finish(format_args!(
                                "{} {:5} {:10} {:>25}:{:<5} {:30} {}",
                                clock.now(&time_format),
                                record.level(),
                                thread.name().unwrap_or("<unknown>"),
                                record.file().unwrap_or("<unknown>"),
                                record.line().unwrap_or(0),
                                record.target(),
                                message,
                            ));
                        }
                        Format::Machine => {
                            let thread = thread::current();
                            out.finish(format_args!(
                                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                                clock.now(&time_format),
                                record.level(),
                                thread.name().unwrap_or("<unknown>"),
                                record.file().unwrap_or("<unknown>"),
                                record.line().unwrap_or(0),
                                record.target(),
                                message,
                            ));
                        }
                        Format::Json => {
                            // We serialize it by putting things into a structure and using serde
                            // for that.
                            //
                            // This is a zero-copy structure.
                            #[derive(Serialize)]
                            struct Msg<'a> {
                                timestamp: Arguments<'a>,
                                level: Arguments<'a>,
                                thread_name: Option<&'a str>,
                                file: Option<&'a str>,
                                line: Option<u32>,
                                target: &'a str,
                                message: &'a Arguments<'a>,
                            }
                            // Unfortunately, the Arguments thing produced by format_args! doesn't
                            // like to live in a variable ‒ all attempts to put it into a let
                            // binding failed with various borrow-checker errors.
                            //
                            // However, constructing it as a temporary when calling a function
                            // seems to work fine. So we use this closure to work around the
                            // problem.
                            let log = |msg: &Msg| {
                                // TODO: Maybe use some shortstring or so here to avoid allocation?
                                let msg = serde_json::to_string(msg)
                                    .expect("Failed to serialize JSON log");
                                out.finish(format_args!("{}", msg));
                            };
                            let thread = thread::current();
                            log(&Msg {
                                timestamp: format_args!("{}", clock.now(&time_format)),
                                level: format_args!("{}", record.level()),
                                thread_name: thread.name(),
                                file: record.file(),
                                line: record.line(),
                                target: record.target(),
                                message,
                            });
                        }
                        Format::Logstash => {
                            // We serialize it by putting things into a structure and using serde
                            // for that.
                            //
                            // This is a zero-copy structure.
                            #[derive(Serialize)]
                            struct Msg<'a> {
                                #[serde(rename = "@timestamp")]
                                timestamp: Arguments<'a>,
                                #[serde(rename = "@version")]
                                version: u8,
                                level: Arguments<'a>,
                                thread_name: Option<&'a str>,
                                logger_name: &'a str,
                                message: &'a Arguments<'a>,
                            }
                            // Unfortunately, the Arguments thing produced by format_args! doesn't
                            // like to live in a variable ‒ all attempts to put it into a let
                            // binding failed with various borrow-checker errors.
                            //
                            // However, constructing it as a temporary when calling a function
                            // seems to work fine. So we use this closure to work around the
                            // problem.
                            let log = |msg: &Msg| {
                                // TODO: Maybe use some shortstring or so here to avoid allocation?
                                let msg = serde_json::to_string(msg)
                                    .expect("Failed to serialize JSON log");
                                out.finish(format_args!("{}", msg));
                            };
                            let thread = thread::current();
                            log(&Msg {
                                timestamp: format_args!("{}", clock.now(&time_format)),
                                version: 1,
                                level: format_args!("{}", record.level()),
                                thread_name: thread.name(),
                                logger_name: record.target(),
                                message,
                            });
                        }
                    }
                });
            }
        }
        match self.destination {
            LogDestination::File { ref filename } => Ok(logger.chain(fern::log_file(filename)?)),
            LogDestination::Syslog { ref host } => {
                let formatter = syslog::Formatter3164 {
                    facility: syslog::Facility::LOG_USER,
                    hostname: host.clone(),
                    // TODO: Does this give us the end-user crate or us?
                    process: env!("CARGO_PKG_NAME").to_owned(),
                    pid: 0,
                };
                // TODO: Other destinations than just unix
                Ok(logger
                    .chain(syslog::unix(formatter).map_err(|e| SyslogError(format!("{}", e)))?))
            }
            LogDestination::Network { ref host, port } => {
                // TODO: Reconnection support
                let conn = TcpStream::connect((&host as &str, port))?;
                Ok(logger.chain(Box::new(conn) as Box<Write + Send>))
            }
            LogDestination::StdOut => Ok(logger.chain(io::stdout())),
            LogDestination::StdErr => Ok(logger.chain(io::stderr())),
        }
    }
}

/// A configuration fragment to set up logging.
///
/// By flattening this into the configuration structure, the program can load options for
/// configuring logging. It adds a new top-level array `logging`. Each item describes one logger,
/// with separate log levels and destination.
///
/// # Logger options
///
/// These are valid for all loggers:
///
/// * `level`: The log level to use. Valid options are `OFF`, `ERROR`, `WARN`, `INFO`, `DEBUG` and
///   `TRACE`.
/// * `per-module`: A map, setting log level overrides for specific modules (logging targets). This
///   one is optional.
/// * `type`: Specifies the type of logger destination. Some of them allow specifying other
///   options.
/// * `clock`: Either `LOCAL` or `UTC`. Defaults to `LOCAL` if not present.
/// * `time_format`: Time
///   [format string](https://docs.rs/chrono/*/chrono/format/strftime/index.html). Defaults to
///   `%+` (which is ISO 8601/RFC 3339). Note that the command line logger (one produced by `-l`)
///   uses a more human-friendly format.
/// * `format`: The format to use. There are few presets (and a custom may come in future).
///   - `message-only`: The line contains only the message itself.
///   - `short`: This is the default. `<timestamp> <level> <target> <message>`. Padded to form
///     columns.
///   - `extended`: <timestamp> <level> <thread-name> <target> <message>`. Padded to form columns.
///   - `full`: `<timestamp> <level> <thread-name> <file>:<line> <target> <message>`. Padded to
///     form columns.
///   - `machine`: Like `full`, but columns are not padded by spaces, they are separated by a
///     single `\t` character, for more convenient processing by tools like `cut`.
///   - `json`: The fields of `full` are encoded into a `json` format, for convenient processing of
///     more modern tools like logstash.
///   - `logstash`: `json` format with fields named and formatted according to
///     [Logback JSON encoder](https://github.com/logstash/logstash-logback-encoder#standard-fields)
///
/// The allowed types are:
/// * `stdout`: The logs are sent to standard output. There are no additional options.
/// * `stderr`: The logs are sent to standard error output. There are no additional options.
/// * `file`: Logs are written to a file. The file is reopened every time a configuration is
///   re-read (therefore every time the application gets `SIGHUP`), which makes it work with
///   logrotate.
///   - `filename`: The path to the file where to put the logs.
/// * `network`: The application connects to a given host and port over TCP and sends logs there.
///   - `host`: The hostname (or IP address) to connect to.
///   - `port`: The port to use.
/// * `syslog`: Sends the logs to syslog. This ignores all the formatting and time options, as
///   syslog handles this itself.
///
/// # Configuration helpers
///
/// This structure works as a configuration helper in three different forms:
///
/// ## No command line options.
///
/// There's no interaction with the command line options. The second parameter of the
/// `config_helper` is set to `()`.
///
/// ```rust
/// #[macro_use]
/// extern crate log;
/// extern crate spirit;
/// extern crate spirit_log;
/// #[macro_use]
/// extern crate serde_derive;
///
/// use spirit::{Empty, Spirit};
/// use spirit_log::Cfg as LogCfg;
///
/// #[derive(Clone, Debug, Default, Deserialize)]
/// struct Cfg {
///     #[serde(flatten)]
///     log: LogCfg,
/// }
///
/// impl Cfg {
///     fn log(&self) -> LogCfg {
///         self.log.clone()
///     }
/// }
///
/// fn main() {
///     Spirit::<Empty, Cfg>::new()
///         .config_helper(Cfg::log, (), "logging")
///         .run(|_spirit| {
///             info!("Hello world");
///             Ok(())
///         });
/// }
/// ```
///
/// ## Basic integration of command line options.
///
/// The second parameter is a closure to extract the [`Opts`](struct.Opts.html) structure from the
/// options (`Fn(&O) -> Opts + Send + Sync + 'static`).
///
/// ## Full customizations
///
/// The second parameter can be the [`Extras`](struct.Extras.html) structure, fully customizing the
/// creation of loggers.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Cfg {
    #[serde(default)]
    logging: Vec<Logger>,
}

fn create<'a, I>(logging: I) -> Result<(LevelFilter, Box<Log>), Error>
where
    I: IntoIterator<Item = &'a Logger>,
{
    debug!("Creating loggers");
    let result = logging
        .into_iter()
        .map(Logger::create)
        .fold_results(Dispatch::new(), Dispatch::chain)?
        .into_log();
    Ok(result)
}

fn install((max_log_level, top_logger): (LevelFilter, Box<Log>)) {
    debug!("Installing loggers");
    log_reroute::reroute_boxed(top_logger);
    log::set_max_level(max_log_level);
}

struct Configured;

impl<O, C> CfgHelper<O, C, Extras<O, C>> for Cfg
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
{
    fn apply<Extractor, Name>(
        mut extractor: Extractor,
        extras: Extras<O, C>,
        name: Name,
        mut builder: Builder<O, C>,
    ) -> Builder<O, C>
    where
        Extractor: FnMut(&C) -> Self + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        let extras = Arc::new(extras);
        let extras_before = Arc::clone(&extras);
        let validator = move |_old_cfg: &Arc<C>, new_cfg: &mut C, opts: &O| -> ValidationResults {
            let opt_logger = extras.opts(opts);
            let logging = extractor(new_cfg);
            let cfg = logging.logging.iter().chain(opt_logger.as_ref());
            let mut loggers = Vec::new();
            let mut max_level = LevelFilter::Off;
            let mut errors = Vec::new();
            match create(cfg) {
                Ok((level, logger)) => {
                    loggers.push(logger);
                    max_level = cmp::max(level, max_level);
                }
                Err(e) => errors.push(e),
            }
            for extra in &extras.loggers {
                match extra(opts, new_cfg) {
                    Ok((level, extra_loggers)) => {
                        loggers.extend(extra_loggers);
                        max_level = cmp::max(level, max_level);
                    }
                    Err(e) => errors.push(e),
                }
            }
            if !errors.is_empty() {
                let errors = errors.into_iter().map(|e| {
                    let with_context = e.context(format!("Can't configure {}", name));
                    ValidationResult::from_error(with_context.into())
                });
                ValidationResults::from(errors)
            } else {
                let install = move || {
                    debug!("Installing loggers");
                    log::set_max_level(max_level);
                    if loggers.len() == 1 {
                        log_reroute::reroute_boxed(loggers.pop().unwrap());
                    } else {
                        log_reroute::reroute(MultiLog(loggers));
                    }
                };
                ValidationResults::from(ValidationResult::nothing().on_success(install))
            }
        };

        let before_config = move |opts: &O| -> Result<(), Error> {
            if let Some(logger) = extras_before.opts(opts) {
                install(create(iter::once(&logger))?);
            }
            Ok(())
        };
        if builder.singleton::<Configured>() {
            let logger = Logger {
                destination: LogDestination::StdErr,
                level: LevelFilter::Warn,
                per_module: HashMap::new(),
                clock: Clock::Local,
                time_format: cmdline_time_format(),
                format: Format::Short,
            };
            let _ = log_reroute::init();
            install(create(iter::once(&logger)).unwrap());
        }
        builder
            .before_config(before_config)
            .config_validator(validator)
    }
}

impl<O, C, E> CfgHelper<O, C, E> for Cfg
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    E: Fn(&O) -> Opts + Send + Sync + 'static,
{
    fn apply<Extractor, Name>(
        extractor: Extractor,
        opt_extractor: E,
        name: Name,
        builder: Builder<O, C>,
    ) -> Builder<O, C>
    where
        Extractor: FnMut(&C) -> Self + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        builder.config_helper(extractor, Extras::with_opts(opt_extractor), name)
    }
}

impl<O, C> CfgHelper<O, C, ()> for Cfg
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
{
    fn apply<Extractor, Name>(
        extractor: Extractor,
        (): (),
        name: Name,
        builder: Builder<O, C>,
    ) -> Builder<O, C>
    where
        Extractor: FnMut(&C) -> Self + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        builder.config_helper(extractor, Extras::default(), name)
    }
}
