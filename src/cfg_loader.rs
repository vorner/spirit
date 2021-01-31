//! Configuration loading.
//!
//! To load the configuration, multiple sources may need to be combined ‒ multiple files,
//! directories with files, command line, environment variables... and may need to be reloaded
//! during the lifetime of an application.
//!
//! The [`Spirit`][crate::Spirit] object (and it's [`Builder`][crate::Builder]) provide high-level
//! semi-automagical management of that. If you do not want to have the full machinery of that, you
//! can use this module to do the loading manually.
//!
//! The lifetime of loading is:
//!
//! 1. Create a [`Builder`][crate::cfg_loader::Builder] with
//!    [`Builder::new`][crate::cfg_loader::Builder::new].
//! 2. Configure the it, using the methods on it.
//! 3. Parse the command line and prepare the loader with
//!    [`build`][crate::cfg_loader::Builder::build] (or, alternatively
//!    [`build_no_opts`][crate::cfg_loader::Builder::build_no_opts] if command line should not be
//!    considered).
//! 4. Load (even as many times as needed) the configuration using
//!    [`load`][crate::cfg_loader::Loader::load].
//!
//! # Examples
//!
//! ```rust
//! use serde::Deserialize;
//! use spirit::{AnyError, Empty};
//! use spirit::cfg_loader::Builder;
//!
//! #[derive(Default, Deserialize)]
//! struct Cfg {
//!     #[serde(default)]
//!     message: String,
//! }
//!
//! fn main() -> Result<(), AnyError> {
//!     let (Empty {}, mut loader) = Builder::new()
//!         .build();
//!     let cfg: Cfg = loader.load()?;
//!     println!("{}", cfg.message);
//!     Ok(())
//! }
//! ```

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::ffi::OsString;
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::path::{Path, PathBuf};

use config::{Config, Environment, File, FileFormat, Source};
use err_context::prelude::*;
use fallible_iterator::FallibleIterator;
use log::{debug, trace, warn};
use serde::de::DeserializeOwned;
use serde::Serialize;
use structopt::clap::{App, Arg};
use structopt::StructOpt;
use toml::Value;

use crate::utils;
use crate::AnyError;

#[derive(Default)]
struct CommonOpts {
    config_overrides: Vec<(String, String)>,

    configs: Vec<PathBuf>,
}

struct OptWrapper<O> {
    common: CommonOpts,
    other: O,
}

// Unfortunately, StructOpt doesn't like flatten with type parameter
// (https://github.com/TeXitoi/structopt/issues/128). It is not even trivial to do, since some of
// the very important functions are *not* part of the trait.
//
// Furthermore, flattening does ugly things to our version and name and description. Therefore, we
// simply manually use clap and enrich all the things here by the right parameters (inspired by
// what structopt for the CommonOpts actually derives).
impl<O: StructOpt> StructOpt for OptWrapper<O> {
    fn clap<'a, 'b>() -> App<'a, 'b> {
        O::clap()
            .arg(
                Arg::with_name("config-overrides")
                    .takes_value(true)
                    .multiple(true)
                    .validator(|s| {
                        utils::key_val(s.as_str())
                            .map(|_: (String, String)| ())
                            .map_err(|e| e.to_string())
                    })
                    .help("Override specific config values")
                    .short("C")
                    .long("config-override")
                    .number_of_values(1),
            )
            .arg(
                Arg::with_name("configs")
                    .takes_value(true)
                    .multiple(true)
                    .help("Configuration files or directories to load"),
            )
    }

    fn from_clap(matches: &structopt::clap::ArgMatches) -> Self {
        let common = CommonOpts {
            config_overrides: matches
                .values_of("config-overrides")
                .map_or_else(Vec::new, |v| {
                    v.map(|s| utils::key_val(s).unwrap()).collect()
                }),
            configs: matches
                .values_of_os("configs")
                .map_or_else(Vec::new, |v| v.map(utils::absolute_from_os_str).collect()),
        };
        OptWrapper {
            common,
            other: StructOpt::from_clap(matches),
        }
    }
}

/// An error returned whenever the user passes something not a file nor a directory as
/// configuration.
#[derive(Clone, Debug)]
pub struct InvalidFileType(PathBuf);

impl Display for InvalidFileType {
    fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
        write!(
            fmt,
            "Configuration path {} is not a file nor a directory",
            self.0.display()
        )
    }
}

impl Error for InvalidFileType {}

/// Returned if configuration path is missing.
#[derive(Clone, Debug)]
pub struct MissingFile(PathBuf);

impl Display for MissingFile {
    fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
        write!(
            fmt,
            "Configuration path {} does not exist",
            self.0.display()
        )
    }
}

impl Error for MissingFile {}

/// Interface for configuring configuration loading options.
///
/// This is the common interface of [`cfg_loader::Builder`][Builder] and [spirit
/// `Builder`][crate::Builder] for configuring how and where from should configuration be loaded.
/// The methods are available on both and do the same thing.
///
/// The interface is also implemented on `Result<ConfigBuilder, AnyError>`. This is both for
/// convenience and for gathering errors to be handled within
/// [`SpiritBuilder::run`][crate::SpiritBuilder::run] in uniform way.
pub trait ConfigBuilder: Sized {
    /// Sets the configuration paths in case the user doesn't provide any.
    ///
    /// This replaces any previously set default paths. If none are specified and the user doesn't
    /// specify any either, no config is loaded (but it is not an error in itself, simply the
    /// defaults will be used, if available).
    ///
    /// This has no effect if the user does provide configuration paths on the command line.
    fn config_default_paths<P, I>(self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>;

    /// Specifies the default configuration.
    ///
    /// This „loads“ the lowest layer of the configuration from the passed string. The expected
    /// format is TOML.
    ///
    /// Any user-provided configuration will be layered on top of it.
    ///
    /// An alternative is to supply the lowest layer through the
    /// [`config_defaults_typed`][ConfigBuilder::config_defaults_typed] ‒ only the last one of the
    /// two wins.
    fn config_defaults<D: Into<String>>(self, config: D) -> Self;

    /// Specifies the default configuration as typed value.
    ///
    /// This is an alternative to [`config_defaults`]. Unlike that
    /// one, this accepts a typed instance of the configuration ‒ the configuration structure.
    ///
    /// The advantage is there's less risk of typos or malformed input and sometimes convenience.
    /// This is, however, less flexible as this needs a *complete* configuration, while
    /// [`config_defaults`] accepts even configurations that are missing some fields. In such case,
    /// defaults would be used (if they are available on the [`Deserialize`][serde::Deserialize]
    /// level) or the configuration would fail if the user provided configuration files don't
    /// contain it.
    ///
    /// Note that pairing between the type passed here and the type of configuration structure
    /// extracted later is not tied together at compile time (as this allows a workaround for the
    /// above described inflexibility, but also because tying them together would be too much work
    /// for very little benefit ‒ it's not likely there would be two different configuration
    /// structures in the same program and got mixed up).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use serde::{Deserialize, Serialize};
    /// use spirit::AnyError;
    /// use spirit::cfg_loader::{Builder, ConfigBuilder};
    ///
    /// #[derive(Default, Deserialize, Serialize)]
    /// struct Cfg {
    ///     #[serde(default)]
    ///     message: String,
    /// }
    ///
    /// fn main() -> Result<(), AnyError> {
    ///     let mut loader = Builder::new()
    ///         .config_defaults_typed(&Cfg {
    ///             message: "hello".to_owned()
    ///         })
    ///         // Expect, as error here would mean a bug, not bad external conditions.
    ///         .expect("Invalid default configuration")
    ///         .build_no_opts();
    ///     let cfg: Cfg = loader.load()?;
    ///     assert_eq!(cfg.message, "hello");
    ///     Ok(())
    /// }
    /// ```
    ///
    /// [`config_defaults`]: ConfigBuilder::config_defaults
    fn config_defaults_typed<C: Serialize>(self, config: &C) -> Result<Self, AnyError> {
        // We internally delegate into serializing to toml and just calling the other method. This
        // is mostly to avoid implementation complexity:
        // * We don't want two different parts of code doing about the same.
        // * This trait isn't parametrized by any type, but we would have to store the structure
        //   inside. `Serialize` is not very friendly to type erasure.
        // * We would have to explicitly handle the situation where both this and config_defaults
        //   gets called, while this way the last-to-be-called wins naturally follows from the
        //   implementation.
        //
        // The overhead should not be interesting, since configuration loading happens only
        // infrequently.

        // A little trick here. Converting a structure to TOML may result in errors if the
        // structure has the „wrong“ order of fields. Putting it into a type-less Value first
        // doesn't suffer from this error and Value outputs its contents in the correct order.
        let untyped = Value::try_from(config)?;
        Ok(self.config_defaults(toml::to_string(&untyped)?))
    }

    /// Enables loading configuration from environment variables.
    ///
    /// If this is used, after loading the normal configuration files, the environment of the
    /// process is examined. Variables with the provided prefix are merged into the configuration.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use serde::Deserialize;
    /// use spirit::{AnyError, Empty};
    /// use spirit::cfg_loader::{Builder, ConfigBuilder};
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
    /// fn main() -> Result<(), AnyError> {
    ///     let (_, mut loader) = Builder::new()
    ///         .config_defaults(DEFAULT_CFG)
    ///         .config_env("HELLO")
    ///         .build::<Empty>();
    ///     let cfg: Cfg = loader.load()?;
    ///     println!("{}", cfg.message);
    ///     Ok(())
    /// }
    /// ```
    ///
    /// If run like this, it'll print `Hi`. The environment takes precedence ‒ even if there was
    /// configuration file and it set the `message`, the `Hi` here would win.
    ///
    /// ```sh
    /// HELLO_MESSAGE="Hi" ./hello
    /// ```
    fn config_env<E: Into<String>>(self, env: E) -> Self;

    /// Configures a config dir filter for a single extension.
    ///
    /// Sets the config directory filter (see [`config_filter`](#method.config_filter)) to one
    /// matching this single extension.
    fn config_ext<E: Into<OsString>>(self, ext: E) -> Self {
        let ext = ext.into();
        self.config_filter(move |path| path.extension() == Some(&ext))
    }

    /// Configures a config dir filter for multiple extensions.
    ///
    /// Sets the config directory filter (see [`config_filter`](#method.config_filter)) to one
    /// matching files with any of the provided extensions.
    fn config_exts<I, E>(self, exts: I) -> Self
    where
        I: IntoIterator<Item = E>,
        E: Into<OsString>,
    {
        let exts = exts.into_iter().map(Into::into).collect::<HashSet<_>>();
        self.config_filter(move |path| {
            path.extension()
                .map(|ext| exts.contains(ext))
                .unwrap_or(false)
        })
    }

    /// Sets the config dir filter for all the supported extensions.
    ///
    /// Note that the list of extensions depends on the enabled features.
    #[allow(clippy::vec_init_then_push)]
    fn config_supported_exts(self) -> Self {
        let mut exts = Vec::new();

        exts.push("toml");
        #[cfg(feature = "json")]
        exts.push("json");
        #[cfg(feature = "yaml")]
        exts.push("yaml");
        #[cfg(feature = "ini")]
        exts.push("ini");
        #[cfg(feature = "hjson")]
        exts.push("hjson");

        self.config_exts(exts)
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
    /// The filter has no effect on files passed directly, only on loading directories. Only files
    /// directly in the directory are loaded ‒ subdirectories are not traversed.
    ///
    /// For more convenient ways to set the filter, see [`config_ext`](#method.config_ext) and
    /// [`config_exts`](#method.config_exts).
    fn config_filter<F: FnMut(&Path) -> bool + Send + 'static>(self, filter: F) -> Self;

    /// Sets if warning should be produced for each unused configuration key.
    ///
    /// If set, a warning message is produced upon loading a configuration for each unused key.
    /// Note that this might not always work reliably.
    ///
    /// The default is true.
    fn warn_on_unused(self, warn: bool) -> Self;
}

impl<C: ConfigBuilder, Error> ConfigBuilder for Result<C, Error> {
    fn config_default_paths<P, I>(self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.map(|c| c.config_default_paths(paths))
    }

    fn config_defaults<D: Into<String>>(self, config: D) -> Self {
        self.map(|c| c.config_defaults(config))
    }

    fn config_env<E: Into<String>>(self, env: E) -> Self {
        self.map(|c| c.config_env(env))
    }

    fn config_filter<F: FnMut(&Path) -> bool + Send + 'static>(self, filter: F) -> Self {
        self.map(|c| c.config_filter(filter))
    }

    fn warn_on_unused(self, warn: bool) -> Self {
        self.map(|c| c.warn_on_unused(warn))
    }
}

/// A builder for the [`Loader`].
///
/// See the [module documentation][crate::cfg_loader] for details about the use.
pub struct Builder {
    default_paths: Vec<PathBuf>,
    defaults: Option<String>,
    env: Option<String>,
    filter: Box<dyn FnMut(&Path) -> bool + Send>,
    warn_on_unused: bool,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

impl Builder {
    /// Creates a new config loader builder.
    pub fn new() -> Self {
        Self {
            default_paths: Vec::new(),
            defaults: None,
            env: None,
            filter: Box::new(|_| false),
            warn_on_unused: true,
        }
    }

    /// The inner part of building, independent of where the options come from.
    fn build_inner(self, opts: CommonOpts) -> Loader {
        let files = if opts.configs.is_empty() {
            self.default_paths
        } else {
            opts.configs
        };
        trace!("Parsed command line arguments");

        Loader {
            files,
            defaults: self.defaults,
            env: self.env,
            filter: self.filter,
            overrides: opts.config_overrides.into_iter().collect(),
            warn_on_unused: self.warn_on_unused,
        }
    }

    /// Turns the builder into the [`Loader`].
    ///
    /// This parses the command line options ‒ the ones specified by the type parameter, enriched
    /// by options related to configuration (paths to config files and config overrides).
    ///
    /// This returns the parsed options and the loader.
    ///
    /// If the command line parsing fails, the application terminates (and prints relevant help).
    pub fn build<O: StructOpt>(self) -> (O, Loader) {
        let opts = OptWrapper::<O>::from_args();
        let loader = self.build_inner(opts.common);
        (opts.other, loader)
    }

    /// Turns this into the [`Loader`], without command line parsing.
    ///
    /// It is similar to [`build`][Builder::build], but doesn't parse the command line, therefore
    /// only the [`config_default_paths`][ConfigBuilder::config_default_paths] are used to
    /// find the config files.
    ///
    /// This is likely useful for tests.
    pub fn build_no_opts(self) -> Loader {
        self.build_inner(Default::default())
    }

    /// Turns this into the [`Loader`], and command line is parsed from the provided iterator.
    ///
    /// Similar to the [`build`][Builder::build], this returns the options and the loader. However,
    /// the options is loaded from the provided iterator and error is explicitly returned. This
    /// makes it better match for tests, but can be useful in other circumstances too.
    ///
    /// Note that the 0th argument is considered to be the name of the application and is not
    /// parsed as an option.
    pub fn build_explicit_opts<O, I>(self, args: I) -> Result<(O, Loader), AnyError>
    where
        O: StructOpt,
        I: IntoIterator,
        I::Item: Into<OsString> + Clone,
    {
        let opts = OptWrapper::<O>::from_iter_safe(args)?;
        let loader = self.build_inner(opts.common);
        Ok((opts.other, loader))
    }
}

impl ConfigBuilder for Builder {
    fn config_defaults<D: Into<String>>(self, config: D) -> Self {
        Self {
            defaults: Some(config.into()),
            ..self
        }
    }

    fn config_default_paths<P, I>(self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        let paths = paths.into_iter().map(Into::into).collect();
        Self {
            default_paths: paths,
            ..self
        }
    }

    fn config_env<E: Into<String>>(self, env: E) -> Self {
        Self {
            env: Some(env.into()),
            ..self
        }
    }

    fn config_filter<F: FnMut(&Path) -> bool + Send + 'static>(self, filter: F) -> Self {
        Self {
            filter: Box::new(filter),
            ..self
        }
    }

    fn warn_on_unused(self, warn: bool) -> Self {
        Self {
            warn_on_unused: warn,
            ..self
        }
    }
}

/// The loader of configuration.
///
/// This is created by the [`Builder`]. See the [module documentation][crate::cfg_loader] for
/// details.
pub struct Loader {
    files: Vec<PathBuf>,
    defaults: Option<String>,
    env: Option<String>,
    overrides: HashMap<String, String>,
    filter: Box<dyn FnMut(&Path) -> bool + Send>,
    warn_on_unused: bool,
}

impl Loader {
    /// Loads configuration according to parameters configured on the originating [`Builder`] and on
    /// the command line.
    ///
    /// Note that it is possible to load the configuration multiple times during the lifetime of
    /// the [`Loader`]. Each time all the sources are loaded from scratch (even new files in
    /// directories are discovered), so this can be used to reflect configuration changes at
    /// runtime.
    pub fn load<C: DeserializeOwned>(&mut self) -> Result<C, AnyError> {
        debug!("Loading configuration");
        let mut config = Config::new();
        let mut external_cfg = false;
        // To avoid problems with trying to parse without any configuration present (it would
        // complain that it found unit and whatever the config was is expected instead).
        config.merge(File::from_str("", FileFormat::Toml))?;
        if let Some(ref defaults) = self.defaults {
            trace!("Loading config defaults");
            config
                .merge(File::from_str(defaults, FileFormat::Toml))
                .context("Failed to read defaults")?;
        }
        for path in &self.files {
            if path.is_file() {
                external_cfg = true;
                trace!("Loading config file {:?}", path);
                config
                    .merge(File::from(path as &Path))
                    .with_context(|_| format!("Failed to load config file {:?}", path))?;
            } else if path.is_dir() {
                trace!("Scanning directory {:?}", path);
                // Take all the file entries passing the config file filter, handling errors on the
                // way.
                let filter = &mut self.filter;
                let mut files = fallible_iterator::convert(path.read_dir()?)
                    .map(|entry| -> Result<Option<PathBuf>, std::io::Error> {
                        let path = entry.path();
                        let meta = path.symlink_metadata()?;
                        if meta.is_file() && (filter)(&path) {
                            Ok(Some(path))
                        } else {
                            trace!("Skipping {:?}", path);
                            Ok(None)
                        }
                    })
                    .filter_map(Ok)
                    .collect::<Vec<_>>()?;
                // Traverse them sorted.
                files.sort();
                for file in files {
                    external_cfg = true;
                    trace!("Loading config file {:?}", file);
                    config
                        .merge(File::from(&file as &Path))
                        .with_context(|_| format!("Failed to load config file {:?}", file))?;
                }
            } else if path.exists() {
                return Err(InvalidFileType(path.to_owned()).into());
            } else {
                return Err(MissingFile(path.to_owned()).into());
            }
        }
        if let Some(env_prefix) = self.env.as_ref() {
            trace!("Loading config from environment {}", env_prefix);
            let env = Environment::with_prefix(env_prefix).separator("_");
            if !external_cfg {
                // We want to know if anything is in the environment, but that means iterating it
                // twice, so do it only if we know we didn't get anything already.
                external_cfg = !env
                    .collect()
                    .context("Failed to include environment in config")?
                    .is_empty();
            }
            config
                .merge(env)
                .context("Failed to include environment in config")?;
        }
        for (ref key, ref value) in &self.overrides {
            trace!("Config override {} => {}", key, value);
            config.set(*key, *value as &str).with_context(|_| {
                external_cfg = true;
                format!("Failed to push override {}={} into config", key, value)
            })?;
        }

        let mut ignored_cback = |ignored: serde_ignored::Path| {
            if self.warn_on_unused {
                warn!("Unused configuration key {}", ignored);
            }
        };
        let config = serde_ignored::Deserializer::new(config, &mut ignored_cback);

        let mut result: Result<_, AnyError> =
            serde_path_to_error::deserialize(config).map_err(|e| {
                let ctx = format!("Failed to decode configuration at {}", e.path());
                e.into_inner().context(ctx).into()
            });

        if !external_cfg {
            result = result
                .context("No config passed to application")
                .map_err(AnyError::from)
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use maplit::hashmap;
    use serde::Deserialize;

    use super::*;
    use crate::Empty;

    #[test]
    fn enum_keys() {
        #[derive(Debug, Deserialize, Eq, PartialEq, Hash)]
        #[serde(rename_all = "kebab-case")]
        enum Key {
            A,
            B,
        }

        #[derive(Debug, Deserialize, Eq, PartialEq)]
        #[serde(rename_all = "kebab-case")]
        struct Cfg {
            map: HashMap<Key, String>,
        }

        const CFG: &str = r#"
            [map]
            a = "hello"
            b = "world"
        "#;

        let cfg: Cfg = Builder::new()
            .config_defaults(CFG)
            .build_no_opts()
            .load()
            .unwrap();

        assert_eq!(
            cfg,
            Cfg {
                map: hashmap! {
                    Key::A => "hello".to_owned(),
                    Key::B => "world".to_owned(),
                }
            }
        );
    }

    #[test]
    fn usize_key() {
        #[derive(Debug, Deserialize, Eq, PartialEq)]
        #[serde(rename_all = "kebab-case")]
        struct Cfg {
            map: HashMap<usize, String>,
        }

        const CFG: &str = r#"
            [map]
            1 = "hello"
            "2" = "world"
        "#;

        let cfg: Cfg = Builder::new()
            .config_defaults(CFG)
            .build_no_opts()
            .load()
            .unwrap();

        assert_eq!(
            cfg,
            Cfg {
                map: hashmap! {
                    1 => "hello".to_owned(),
                    2 => "world".to_owned(),
                }
            }
        );
    }

    #[test]
    fn str_to_int() {
        #[derive(Debug, Deserialize, Eq, PartialEq)]
        #[serde(rename_all = "kebab-case")]
        struct Cfg {
            value: usize,
        }

        // Here the value is encoded as string. The config crate should handle that.
        // This happens for example when we do overrides from command line.
        const CFG: &str = r#"value = "42""#;

        let cfg: Cfg = Builder::new()
            .config_defaults(CFG)
            .build_no_opts()
            .load()
            .unwrap();

        assert_eq!(cfg, Cfg { value: 42 });
    }

    #[test]
    fn cmd_overrides() {
        #[derive(Debug, Deserialize, Eq, PartialEq)]
        #[serde(rename_all = "kebab-case")]
        struct Cfg {
            value: usize,
        }

        #[derive(Debug, Eq, PartialEq, StructOpt)]
        struct Opts {
            #[structopt(short = "o")]
            option: bool,
        }

        const CFG: &str = r#"
            value = 42
        "#;

        let (opts, mut loader): (Opts, Loader) = Builder::new()
            .config_defaults(CFG)
            .build_explicit_opts(vec!["my-app", "-o", "-C", "value=12"])
            .unwrap();

        assert_eq!(opts, Opts { option: true });

        let cfg: Cfg = loader.load().unwrap();

        assert_eq!(cfg, Cfg { value: 12 });
    }

    #[test]
    fn combine_dir() {
        #[derive(Debug, Deserialize, Eq, PartialEq)]
        struct Cfg {
            value: usize,
            option: bool,
            another: String,
        }

        const CFG: &str = r#"
            value = 42
            another = "Hello"
        "#;

        let (Empty {}, mut loader) = Builder::new()
            .config_supported_exts()
            .config_defaults(CFG)
            .build_explicit_opts(vec!["my-app", "tests/data"])
            .unwrap();

        let cfg: Cfg = loader.load().unwrap();
        assert_eq!(
            cfg,
            Cfg {
                value: 12,                   // Overridden from tests/data/cfg1.yaml
                option: true,                // From tests/data/cfg2.toml
                another: "Hello".to_owned(), // From the defaults
            }
        );
    }
}
