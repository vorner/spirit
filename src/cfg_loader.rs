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
//! 2. Configure the it.
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
//! use failure::Error;
//! use serde::Deserialize;
//! use spirit::Empty;
//! use spirit::cfg_loader::Builder;
//!
//! #[derive(Default, Deserialize)]
//! struct Cfg {
//!     #[serde(default)]
//!     message: String,
//! }
//!
//! fn main() -> Result<(), Error> {
//!     let (Empty {}, mut loader) = Builder::new()
//!         .build();
//!     let cfg: Cfg = loader.load()?;
//!     println!("{}", cfg.message);
//!     Ok(())
//! }
//! ```

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use config::{Config, Environment, File, FileFormat};
use failure::{bail, Error, Fail, ResultExt};
use fallible_iterator::FallibleIterator;
use log::{debug, trace};
use serde::de::DeserializeOwned;
use structopt::clap::App;
use structopt::StructOpt;
use toml::Value;

#[derive(StructOpt)]
struct CommonOpts {
    /// Override specific config values.
    #[structopt(
        short = "C",
        long = "config-override",
        parse(try_from_str = "crate::utils::key_val"),
        raw(number_of_values = "1")
    )]
    config_overrides: Vec<(String, String)>,

    /// Configuration files or directories to load.
    #[structopt(parse(from_os_str = "crate::utils::absolute_from_os_str"))]
    configs: Vec<PathBuf>,
}

struct OptWrapper<O> {
    common: CommonOpts,
    other: O,
}

// Unfortunately, StructOpt doesn't like flatten with type parameter
// (https://github.com/TeXitoi/structopt/issues/128). It is not even trivial to do, since some of
// the very important functions are *not* part of the trait. So we do it manually ‒ we take the
// type parameter's clap definition and add our own into it.
impl<O: StructOpt> StructOpt for OptWrapper<O> {
    fn clap<'a, 'b>() -> App<'a, 'b> {
        CommonOpts::augment_clap(O::clap())
    }

    fn from_clap(matches: &structopt::clap::ArgMatches) -> Self {
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

pub trait ConfigBuilder: Sized {
    /// Sets the configuration paths in case the user doesn't provide any.
    ///
    /// This replaces any previously set default paths. If none are specified and the user doesn't
    /// specify any either, no config is loaded (but it is not an error, simply the defaults will
    /// be used, if available).
    fn config_default_paths<P, I>(self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>;

    /// Specifies the default configuration.
    ///
    /// This „loads“ the lowest layer of the configuration from the passed string. The expected
    /// format is TOML.
    fn config_defaults<D: Into<String>>(self, config: D) -> Self;

    /// Enables loading configuration from environment variables.
    ///
    /// If this is used, after loading the normal configuration files, the environment of the
    /// process is examined. Variables with the provided prefix are merged into the configuration.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use failure::Error;
    /// use serde::Deserialize;
    /// use spirit::cfg_loader::{Builder, ConfigBuilder};
    /// use spirit::Empty;
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
    /// fn main() -> Result<(), Error> {
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
    fn config_filter<F: FnMut(&Path) -> bool + Send + 'static>(self, filter: F) -> Self;
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
}

/// A builder for [`Loader`].
///
/// See the [module documentation](index.html) for details about the use.
pub struct Builder {
    default_paths: Vec<PathBuf>,
    defaults: Option<String>,
    env: Option<String>,
    filter: Box<FnMut(&Path) -> bool + Send>,
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
        }
    }

    /// Turn the builder into the [`Loader`].
    ///
    /// This parses the command line options ‒ the ones specified by the type parameter, enriched
    /// by options related to configuration (paths to config files and config overrides).
    ///
    /// This returns the parsed options and the loader.
    ///
    /// If the command line parsing fails, the application terminates (and prints relevant help).
    pub fn build<O: StructOpt>(self) -> (O, Loader) {
        let opts = OptWrapper::<O>::from_args();
        let files = if opts.common.configs.is_empty() {
            self.default_paths
        } else {
            opts.common.configs
        };
        debug!("Parsed command line arguments");

        let loader = Loader {
            files,
            defaults: self.defaults,
            env: self.env,
            filter: self.filter,
            overrides: opts.common.config_overrides.into_iter().collect(),
        };
        (opts.other, loader)
    }

    /// Turns this into the [`Loader`], without command line parsing.
    ///
    /// It is similar to [`build`][Builder::build], but doesn't parse the command line, therefore
    /// only the [`default_paths`][Builder::default_paths] are used to find the config files.
    ///
    /// This is likely useful for tests.
    pub fn build_no_opts(self) -> Loader {
        debug!("Created cfg loader without command line");
        Loader {
            files: self.default_paths,
            defaults: self.defaults,
            env: self.env,
            filter: self.filter,
            overrides: HashMap::new(),
        }
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
}

/// The loader of configuration.
///
/// This is created by the [`Builder`]. See the [module documentation](index.html) for details.
pub struct Loader {
    files: Vec<PathBuf>,
    defaults: Option<String>,
    env: Option<String>,
    overrides: HashMap<String, String>,
    filter: Box<FnMut(&Path) -> bool + Send>,
}

impl Loader {
    /// Load configuration according to parameters configured on the originating [`Builder`] and on
    /// the command line.
    pub fn load<C: DeserializeOwned>(&mut self) -> Result<C, Error> {
        debug!("Loading configuration");
        let mut config = Config::new();
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
        if let Some(env_prefix) = self.env.as_ref() {
            trace!("Loading config from environment {}", env_prefix);
            config
                .merge(Environment::with_prefix(env_prefix).separator("_"))
                .context("Failed to include environment in config")?;
        }
        for (ref key, ref value) in &self.overrides {
            trace!("Config override {} => {}", key, value);
            config.set(*key, *value as &str).with_context(|_| {
                format!("Failed to push override {}={} into config", key, value)
            })?;
        }

        // A hack: the config crate has a lot of glitches when it comes to deserializing ‒
        // unhelpful error messages, enums as keys can't be decoded, etc. So we turn it into
        // toml::Value (which should not fail) and then use that one to do the actual decoding. The
        // latter is much better quality, so we get better error messages.
        let intermediate: Value = config.try_into().context("Failed to decode configuration")?;

        let result = intermediate.try_into().context("Failed to decode configuration")?;

        Ok(result)
    }
}
