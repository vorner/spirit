#![doc(
    html_root_url = "https://docs.rs/spirit-log/0.1.0/spirit-log/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
// #![warn(missing_docs)] TODO

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
extern crate spirit;
#[allow(unused_imports)]
#[macro_use]
extern crate structopt;
extern crate syslog;

use std::cmp;
use std::collections::HashMap;
use std::fmt::{Debug, Display};
use std::io::{self, Write};
use std::iter;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Local;
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
        })
    }
}

type ExtraLogger<O, C> =
    Box<Fn(&O, &C) -> Result<(LevelFilter, Vec<Box<Log>>), Error> + Send + Sync>;

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
    pub fn with_opts<F: Fn(&O) -> Opts + Send + Sync + 'static>(extractor: F) -> Self {
        Extras {
            opts: Some(Box::new(extractor)),
            loggers: Vec::new(),
        }
    }
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
pub enum LogDestination {
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
    // TODO: Format
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
        match self.destination {
            // We don't want to format syslog
            LogDestination::Syslog { .. } => (),
            // We do with the other things
            _ => {
                logger = logger.format(|out, message, record| {
                    out.finish(format_args!(
                        "{} {:5} {:30} {}",
                        Local::now().format("%Y-%m-%d %H:%M:%S:%.3f"),
                        record.level(),
                        record.target(),
                        message,
                    ))
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
                let errors = errors
                    .into_iter()
                    .map(|e| ValidationResult::error(format!("{}: {}", name, e)));
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
