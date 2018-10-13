use std::collections::HashMap;
use std::io::{self, Write};
use std::net::TcpStream;
use std::path::PathBuf;

use chrono::Local;
use failure::Error;
use fern::{self, Dispatch};
use itertools::Itertools;
use log::{self, LevelFilter, Log};
use log_reroute;
use serde::de::{Deserialize, Deserializer, Error as DeError};
use syslog;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")] // TODO: Make deny-unknown-fields work
pub(crate) enum LogDestination {
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

fn deserialize_level_filter<'de, D: Deserializer<'de>>(d: D) -> Result<LevelFilter, D::Error> {
    let s = String::deserialize(d)?;
    s.parse().map_err(|_| {
        D::Error::unknown_variant(&s, &["OFF", "ERROR", "WARN", "INFO", "DEBUG", "TRACE"])
    })
}

fn default_level_filter() -> LevelFilter {
    LevelFilter::Error
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

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")] // TODO: Make deny-unknown-fields work
pub(crate) struct Logging {
    #[serde(flatten)]
    pub(crate) destination: LogDestination,
    #[serde(
        default = "default_level_filter",
        deserialize_with = "deserialize_level_filter"
    )]
    pub(crate) level: LevelFilter,
    #[serde(default, deserialize_with = "deserialize_per_module")]
    pub(crate) per_module: HashMap<String, LevelFilter>,
    // TODO: Format
}

impl Logging {
    pub(crate) fn create(&self) -> Result<Dispatch, Error> {
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

pub(crate) fn create<'a, I>(logging: I) -> Result<(LevelFilter, Box<Log>), Error>
where
    I: IntoIterator<Item = &'a Logging>,
{
    let result = logging
        .into_iter()
        .map(Logging::create)
        .fold_results(Dispatch::new(), Dispatch::chain)?
        .into_log();
    Ok(result)
}

pub(crate) fn install((max_log_level, top_logger): (LevelFilter, Box<Log>)) {
    log_reroute::reroute_boxed(top_logger);
    log::set_max_level(max_log_level);
}
