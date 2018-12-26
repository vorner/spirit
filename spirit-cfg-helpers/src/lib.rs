#![doc(
    html_root_url = "https://docs.rs/spirit-cfg-helpers/0.1.0/spirit_cfg_helpers/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! Spirit configuration helpers
//!
//! These helpers are meant to integrate into the [spirit] configuration framework. They aim at
//! making the user experience around application's configuration more smooth for the user.
//! Specifically, they allow dumping the configuration collected through all the config files and
//! environment variables and printing help about all the configuration options the application
//! accepts.
//!
//! # Features
//!
//! By default, all features are turned on. However, it is possible to opt out of some to cut down
//! on dependencies. Specifically:
//!
//! * `toml` and `json` features enable dumping in the respective formats.
//! * `cfg-help` enables the printing of configuration help.
//!
//! [spirit]: https://crates.io/crates/spirit

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

/// A helper to log changes of configuration every time it is reloaded.
///
/// This is useful when examining logs, to know with which configuration a problem happened.
///
/// # Parameters
///
/// * `level`: On which log level the logging should happen.
/// * `opts_too`: If set to `true`, the log message will contain the command line options as well
///   as the configuration.
///
/// # Notes
///
/// To mask passwords from logs, use the [`spirit::utils::Hidden`] wrapper.
///
/// # Examples
///
/// ```rust
/// use log::Level;
/// use spirit::{Empty, Spirit};
///
/// fn main() {
///     Spirit::<Empty, Empty>::new()
///         .with(spirit_cfg_helpers::config_logging(Level::Info, true))
///         .run(|_| Ok(()));
/// }
/// ```
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
struct DumpFormatParseError(String);

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

/// A command line fragment add `--dump-config` to allow showing loaded configuration.
///
/// When this is added into the command line options structure, the `--dump-config` and
/// `--dump-config-as` options are added.
///
/// These dump the current configuration and exit.
///
/// In case the configuration is collected over multiple configuration files, directories and
/// possibly environment variables and command line overrides, it is not always clear what exact
/// configuration is actually used. This allows the user to query the actual configuration the
/// application would use.
///
/// The fragment can be either used manually with the [`dump`][CfgDump::dump] method or
/// automatically by registering its [`helper`][CfgDump::helper].
///
/// # Requirements
///
/// For this to work, the configuration structure must implement [`Serialize`]. This is not
/// mandated by [`Spirit`][spirit::Spirit] itself. However, all the fragments provided by spirit
/// crates implement it. For custom structures it is often sufficient to stick
/// `#[derive(Serialize)]` onto them.
///
/// # Examples
///
/// ```rust
/// use serde_derive::{Deserialize, Serialize};
/// use spirit::Spirit;
/// use spirit_cfg_helpers::CfgDump;
/// use structopt::StructOpt;
///
/// #[derive(Default, Deserialize, Serialize)]
/// struct Cfg {
///     option: Option<String>,
/// }
///
/// #[derive(Debug, StructOpt)]
/// struct Opts {
///     #[structopt(flatten)]
///     dump: CfgDump,
/// }
///
/// impl Opts {
///     fn dump(&self) -> &CfgDump {
///         &self.dump
///     }
/// }
///
/// fn main() {
///     Spirit::<Opts, Cfg>::new()
///         .with(CfgDump::helper(Opts::dump))
///         .run(|_| Ok(()));
/// }
/// ```
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
    /// Dump configuration if it is asked for in the options.
    ///
    /// If the parsed options specify to dump the configuration, this does so and exits. If the
    /// options don't specify that, it does nothing.
    ///
    /// This can be used manually. However, the common way is to register the
    /// [`helper`][CfgDump::helper] within a [`Builder`] and let it do everything automatically.
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

    /// A helper that can be registered with [`Builder::with`].
    ///
    /// The parameter is an extractor, a function that takes the whole command line options
    /// structure and returns a reference to just the [`CfgDump`] instance in there.
    ///
    /// Note that for configuration to be dumped, it needs to be parsed first.
    ///
    /// Also, as this exits if the dumping is requested, it makes some sense to register it sooner
    /// than later. It registers itself as a [`config_validator`][Builder::config_validator] and it
    /// is not needed to validate parts of the configuration only to throw it out on the exit.
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

    /// A command line options fragment to add the `--help-config` option.
    ///
    /// For the user to be able to configure an application, the user needs to know what options
    /// can be configured. Usually, this is explained using an example configuration file or through
    /// a manually written documentation. However, maintaining either is a lot of work, not
    /// mentioning that various [spirit] crates provide configuration fragments composed from
    /// several type parameters so hunting down all the available options might be hard.
    ///
    /// This helper uses the [`StructDoc`] trait to extract the structure and documentation of the
    /// configuration automatically. Usually, its derive will extract description from fields' doc
    /// comments. See the [structdoc] crate's documentation to know how to let the documentation be
    /// created semi-automatically. All the configuration fragments provided by the spirit crates
    /// implement [`StructDoc`], unless their [`cfg-help`] feature is disabled.
    ///
    /// When the `--help-config` is specified, this auto-generated documentation is printed and the
    /// application exits.
    ///
    /// The fragment can be used either manually with the [`help`][CfgHelp::help] method or by
    /// registering the [`helper`][CfgHelp::helper] within a [`Builder`][Builder::with].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use serde_derive::Deserialize;
    /// use spirit::Spirit;
    /// use spirit_cfg_helpers::CfgHelp;
    /// use structdoc::StructDoc;
    /// use structopt::StructOpt;
    ///
    /// #[derive(Default, Deserialize, StructDoc)]
    /// struct Cfg {
    ///     /// A very much useless but properly documented option.
    /// #   #[allow(dead_code)]
    ///     option: Option<String>,
    /// }
    ///
    /// #[derive(Debug, StructOpt)]
    /// struct Opts {
    ///     #[structopt(flatten)]
    ///     help: CfgHelp,
    /// }
    ///
    /// impl Opts {
    ///     fn help(&self) -> &CfgHelp {
    ///         &self.help
    ///     }
    /// }
    ///
    /// fn main() {
    ///     Spirit::<Opts, Cfg>::new()
    ///         .with(CfgHelp::helper(Opts::help))
    ///         .run(|_| Ok(()));
    /// }
    /// ```
    #[derive(Clone, Debug, Default, StructOpt)]
    pub struct CfgHelp {
        /// Provide help about possible configuration options and exit.
        #[structopt(long = "--help-config")]
        config_help: bool,
        // TODO: Once StructDoc implements some finer-grained control, expose it too.
    }

    impl CfgHelp {
        /// Show the help and exit if it was specified as an option.
        ///
        /// This can be called manually to check for the command line option at the right time. If
        /// it was specified, the help for the `C` type is printed and the application exits. If
        /// the command line was not specified, this does nothing.
        ///
        /// Note that the `C` type is passed as type parameter, therefore this needs to be invoked
        /// with the turbofish syntax.
        ///
        /// The preferred way is usually by registering the [`helper`][CfgHelp::helper].
        ///
        /// # Examples
        ///
        /// ```rust
        /// use serde_derive::Deserialize;
        /// use spirit_cfg_helpers::CfgHelp;
        /// use structdoc::StructDoc;
        /// use structopt::StructOpt;
        ///
        /// #[derive(Deserialize, StructDoc)]
        /// struct Cfg {
        ///     /// A very much useless but properly documented option
        /// #   #[allow(dead_code)]
        ///     option: Option<String>,
        /// }
        ///
        /// #[derive(StructOpt)]
        /// struct Opts {
        ///     #[structopt(flatten)]
        ///     help: CfgHelp,
        /// }
        ///
        /// let opts = Opts::from_args();
        /// opts.help.help::<Cfg>();
        /// ```
        pub fn help<C: StructDoc>(&self) {
            if self.config_help {
                println!("{}", C::document());
                process::exit(0);
            }
        }

        /// A helper to be registered within a [`Builder`][Builder::with].
        ///
        /// The extractor should take the whole command line options structure and provide
        /// reference to just the [`CfgHelp`] instance.
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

    /// A combination of the [`CfgDump`] and [`CfgHelp`] fragments.
    ///
    /// This is simply a combination of both fragments, providing the same options and
    /// functionality. Usually one wants to use both. This saves a bit of code, as only one field
    /// and one helper needs to be registered.
    ///
    /// # Requirements
    ///
    /// For this to work, the configuration structure needs to implement both [`Serialize`] and
    /// [`StructDoc`].
    #[derive(Clone, Debug, Default, StructOpt)]
    pub struct Opts {
        #[structopt(flatten)]
        config_dump: CfgDump,

        #[structopt(flatten)]
        config_help: CfgHelp,
    }

    impl Opts {
        /// The helper to be registered within a [`Builder`][Builder::with].
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
