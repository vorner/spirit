use std::fmt::Debug;
use std::process;
use std::str::FromStr;
use std::sync::Arc;

use failure::Fail;
use log::{log, Level};
use serde::de::DeserializeOwned;
use serde::Serialize;
use spirit::helpers::Helper;
use spirit::validation::Result as ValidationResult;
use spirit::Builder;
use structopt::StructOpt;

pub fn config_logging<O, C>(level: Level, opts_too: bool) -> impl Helper<O, C>
where
    O: Debug + StructOpt + Send + Sync + 'static,
    C: Debug + DeserializeOwned + Send + Sync + 'static,
{
    move |builder: Builder<O, C>| {
        builder.on_config(move |opts, cfg| {
            if opts_too {
                log!(
                    level,
                    "Using cmd-line options {:?} and configuration {:?}",
                    opts,
                    cfg
                );
            } else {
                log!(level, "Using configuration {:?}", cfg);
            }
        })
    }
}

#[derive(Debug, Fail)]
#[fail(display = "Invalid config format {}", _0)]
pub struct DumpFormatParseError(String);

#[derive(Copy, Clone, Debug)]
enum DumpFormat {
    Toml,
    #[cfg(feature = "json")]
    Json,
    #[cfg(feature = "yaml")]
    Yaml,
}

impl DumpFormat {
    fn dump<C: Serialize>(self, cfg: &C) {
        let dump = match self {
            DumpFormat::Toml => {
                toml::to_string_pretty(cfg).expect("Dirty stuff in config, can't dump")
            }
            #[cfg(feature = "json")]
            DumpFormat::Json => {
                serde_json::to_string_pretty(cfg).expect("Dirty stuff in config, can't dump")
            }
            #[cfg(feature = "yaml")]
            DumpFormat::Yaml => {
                serde_yaml::to_string(cfg).expect("Dirty stuff in config, can't dump")
            }
        };
        println!("{}", dump);
    }
}

impl FromStr for DumpFormat {
    type Err = DumpFormatParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "toml" => Ok(DumpFormat::Toml),
            #[cfg(feature = "json")]
            "json" => Ok(DumpFormat::Json),
            #[cfg(feature = "yaml")]
            "yaml" => Ok(DumpFormat::Yaml),
            s => Err(DumpFormatParseError(s.to_owned())),
        }
    }
}

#[derive(Clone, Debug, Default, StructOpt)]
pub struct CfgDump {
    /// Dump the parsed configuration and exit.
    #[structopt(long = "--dump-config")]
    dump_config: bool,

    /// Dump the parsed configuration and exit.
    ///
    /// Allows choosing the format to dump in: toml
    #[cfg_attr(feature = "json", doc = "json")]
    #[cfg_attr(feature = "yaml", doc = "yaml")]
    #[structopt(long = "--dump-config-as")]
    dump_config_as: Option<DumpFormat>,
}

impl CfgDump {
    pub fn dump<C: Serialize>(&self, cfg: &C) {
        if let Some(format) = self.dump_config_as {
            format.dump(cfg);
        } else if self.dump_config {
            DumpFormat::Toml.dump(cfg);
        } else {
            return;
        }
        process::exit(0);
    }

    pub fn helper<O, C, F>(extract: F) -> impl Helper<O, C>
    where
        F: FnOnce(&O) -> &Self + Send + 'static,
        O: Debug + StructOpt + Send + Sync + 'static,
        C: DeserializeOwned + Serialize + Send + Sync + 'static,
    {
        let mut extract = Some(extract);
        let validator = move |_: &_, cfg: &mut C, opts: &O| {
            if let Some(extract) = extract.take() {
                let me = extract(opts);
                me.dump(cfg);
            }
            ValidationResult::nothing()
        };
        |builder: Builder<O, C>| builder.config_validator(validator)
    }
}

#[cfg(feature = "cfg-help")]
mod cfg_help {
    use super::*;

    use structdoc::StructDoc;

    #[derive(Clone, Debug, Default, StructOpt)]
    pub struct CfgHelp {
        /// Provide help about possible configuration options and exit.
        #[structopt(long = "--help-config")]
        config_help: bool,
        // TODO: Once StructDoc implements some finer-grained control, expose it too.
    }

    impl CfgHelp {
        pub fn help<C: StructDoc>(&self) {
            if self.config_help {
                println!("{}", C::document());
                process::exit(0);
            }
        }

        pub fn helper<O, C, F>(extract: F) -> impl Helper<O, C>
        where
            F: FnOnce(&O) -> &Self + Send + 'static,
            O: Debug + StructOpt + Send + Sync + 'static,
            C: DeserializeOwned + StructDoc + Send + Sync + 'static,
        {
            |builder: Builder<O, C>| {
                builder.before_config(|opts: &O| {
                    extract(opts).help::<C>();
                    Ok(())
                })
            }
        }
    }

    #[derive(Clone, Debug, Default, StructOpt)]
    pub struct Opts {
        #[structopt(flatten)]
        config_dump: CfgDump,

        #[structopt(flatten)]
        config_help: CfgHelp,
    }

    impl Opts {
        pub fn helper<O, C, F>(extract: F) -> impl Helper<O, C>
        where
            F: Fn(&O) -> &Self + Send + Sync + 'static,
            O: Debug + StructOpt + Send + Sync + 'static,
            C: DeserializeOwned + Serialize + StructDoc + Send + Sync + 'static,
        {
            let extract_dump = Arc::new(extract);
            let extract_help = Arc::clone(&extract_dump);
            |builder: Builder<O, C>| {
                builder
                    .with(CfgDump::helper(move |opts| &extract_dump(opts).config_dump))
                    .with(CfgHelp::helper(move |opts| &extract_help(opts).config_help))
            }
        }
    }
}

#[cfg(feature = "cfg-help")]
pub use crate::cfg_help::{CfgHelp, Opts};
