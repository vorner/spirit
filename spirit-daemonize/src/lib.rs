#![doc(
    html_root_url = "https://docs.rs/spirit-daemonize/0.2.1/spirit_daemonize/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! A spirit extension for daemonization.
//!
//! The configuration in here extends the [`spirit`](https://crates.io/crates/spirit) configuration
//! framework to automatically go into background based on user's configuration and command line
//! options.
//!
//! # Examples
//!
//! ```rust
//! extern crate spirit;
//! extern crate spirit_daemonize;
//! #[macro_use]
//! extern crate serde_derive;
//! #[macro_use]
//! extern crate structopt;
//!
//! use spirit::prelude::*;
//! use spirit_daemonize::{Daemon, Opts as DaemonOpts};
//!
//! // From config files
//! #[derive(Default, Deserialize)]
//! struct Cfg {
//!     #[serde(default)]
//!     daemon: Daemon,
//! }
//!
//! impl Cfg {
//!    fn daemon(&self, opts: &Opts) -> Daemon {
//!        opts.daemon.transform(self.daemon.clone())
//!    }
//! }
//!
//! // From command line
//! #[derive(Debug, StructOpt)]
//! struct Opts {
//!     #[structopt(flatten)]
//!     daemon: DaemonOpts,
//! }
//!
//! fn main() {
//!      Spirit::<Opts, Cfg>::new()
//!         .with(Daemon::extension(Cfg::daemon))
//!         .run(|_spirit| {
//!             // Possibly daemonized program goes here
//!             Ok(())
//!         });
//! }
//! ```
//!
//! # Added options
//!
//! The program above gets the `-d` command line option, which enables daemonization. Furthermore,
//! the configuration now understands a new `daemon` section, with these options:
//!
//! * `user`: The user to become. Either a numeric ID or name. If not present, it doesn't change the
//!   user.
//! * `group`: Similar as user, but with group.
//! * `pid-file`: A pid file to write on startup. If not present, nothing is stored.
//! * `workdir`: A working directory it'll switch into. If not set, defaults to `/`.
//! * `daemonize`: Should this go into background or not? If combined with the
//!   [`Opts`](struct.Opts.html), it can be overridden on command line.
//!
//! # Multithreaded applications
//!
//! As daemonization is done by using `fork`, you should start any threads *after* you initialize
//! the `spirit`. Otherwise you'll lose the threads (and further bad things will happen).
//!
//! The daemonization happens inside the `config_validator` callback. If other config validators
//! need to start any threads, they should be plugged in after the daemonization callback. However,
//! the safer option is to start them inside the `run` method.

extern crate failure;
#[macro_use]
extern crate log;
extern crate nix;
extern crate privdrop;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate spirit;
#[cfg(feature = "cfg-help")]
#[macro_use]
extern crate structdoc;
// For some reason, this produces a warning about unused on nightly… but it is needed on stable
#[allow(unused_imports)]
#[macro_use]
extern crate structopt;

use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;

use failure::{Error, ResultExt};
use nix::sys::stat::{self, Mode};
use nix::unistd::{self, ForkResult, Gid, Uid};
use spirit::extension::{Extensible, Extension};
use spirit::validation::Action;
use structopt::StructOpt;

/// Configuration of either user or a group.
///
/// This is used to load the configuration into which user and group to drop privileges.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[serde(untagged)]
pub enum SecId {
    /// Look up based on the name.
    Name(String),
    /// Use the numerical value directly.
    Id(u32),
    /// Don't drop privileges.
    ///
    /// This is not read from configuration, but it is the default value available if nothing is
    /// listed in configuration.
    #[serde(skip)]
    Nothing,
}

impl SecId {
    fn is_nothing(&self) -> bool {
        self == &SecId::Nothing
    }
}

impl Default for SecId {
    fn default() -> Self {
        SecId::Nothing
    }
}

/// A configuration fragment for configuration of daemonization.
///
/// This describes how to go into background with some details.
///
/// The fields can be manipulated by the user of this crate. However, it is not possible to create
/// the struct manually. This is on purpose, some future versions might add more fields. If you
/// want to create one, use `Daemon::default` and modify certain fields as needed.
///
/// # Examples
///
/// ```rust
/// # use spirit_daemonize::Daemon;
/// let mut daemon = Daemon::default();
/// daemon.workdir = Some("/".into());
/// ```
///
/// # See also
///
/// If you want to daemonize, but not to switch users (or allow switching users), either because
/// the daemon needs to keep root privileges or because it is expected to be already started as
/// ordinary user, use the [`UserDaemon`](struct.UserDaemon.html) instead.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[serde(rename_all = "kebab-case")]
pub struct Daemon {
    /// The user to drop privileges to.
    ///
    /// The user is not changed if not provided.
    #[serde(default, skip_serializing_if = "SecId::is_nothing")]
    pub user: SecId,

    /// The group to drop privileges to.
    ///
    /// The group is not changed if not provided.
    #[serde(default, skip_serializing_if = "SecId::is_nothing")]
    pub group: SecId,

    /// Where to store a PID file.
    ///
    /// If not set, no PID file is created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid_file: Option<PathBuf>,

    /// Switch to this working directory at startup.
    ///
    /// If not set, working directory is not switched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workdir: Option<PathBuf>,

    // This is overwritten by [`Opts::transform`](struct.Opts.html#method.transform).
    //
    /// Enable the daemonization.
    ///
    /// Even if this is false, some activity (changing users, setting PID file, etc) is still done,
    /// but it doesn't go to background.
    #[serde(default)]
    pub daemonize: bool,

    // Prevent the user from creating this directly.
    #[serde(default, skip)]
    sentinel: (),
}

impl Daemon {
    /// Goes into background according to the configuration.
    ///
    /// This does the actual work of daemonization. This can be used manually.
    pub fn daemonize(&self) -> Result<(), Error> {
        // TODO: Tests for this
        debug!("Preparing to daemonize with {:?}", self);
        stat::umask(Mode::empty()); // No restrictions on write modes
        let workdir = self
            .workdir
            .as_ref()
            .map(|pb| pb as &Path)
            .unwrap_or_else(|| Path::new("/"));
        trace!("Changing working directory to {:?}", workdir);
        env::set_current_dir(workdir)?;
        if self.daemonize {
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
            if let ForkResult::Parent { .. } = unistd::fork()? {
                process::exit(0);
            }
        } else {
            trace!("Not going to background");
        }
        if let Some(ref file) = self.pid_file {
            let mut f = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o644)
                .open(file)?;
            writeln!(f, "{}", unistd::getpid())?;
        }
        // PrivDrop implements the necessary libc lookups to find the group and
        //  user entries matching the given names. If these queries fail,
        //  because the user or group names are invalid, the function will fail.
        match self.group {
            SecId::Id(id) => unistd::setgid(Gid::from_raw(id))?,
            SecId::Name(ref name) => privdrop::PrivDrop::default().group(&name).apply()?,
            SecId::Nothing => (),
        }
        match self.user {
            SecId::Id(id) => unistd::setuid(Uid::from_raw(id))?,
            SecId::Name(ref name) => privdrop::PrivDrop::default().user(&name).apply()?,
            SecId::Nothing => (),
        }
        Ok(())
    }

    /// An extension that can be plugged into the [`Spirit`][spirit::Spirit].
    ///
    /// See the [crate documentation](index.html).
    pub fn extension<E, F>(extractor: F) -> impl Extension<E>
    where
        E: Extensible<Ok = E>,
        F: Fn(&E::Config, &E::Opts) -> Self + Send + 'static,
    {
        let mut previous_daemon = None;
        let validator_hook =
            move |_: &_, cfg: &Arc<E::Config>, opts: &_| -> Result<Action, Error> {
                let daemon = extractor(cfg, opts);
                if let Some(previous) = previous_daemon.as_ref() {
                    if previous != &daemon {
                        warn!("Can't change daemon config at runtime");
                    }
                    return Ok(Action::new());
                }
                daemon.daemonize().context("Failed to daemonize")?;
                previous_daemon = Some(daemon);
                Ok(Action::new())
            };
        move |e: E| e.config_validator(validator_hook)
    }
}

impl From<UserDaemon> for Daemon {
    fn from(ud: UserDaemon) -> Daemon {
        Daemon {
            pid_file: ud.pid_file,
            workdir: ud.workdir,
            daemonize: ud.daemonize,
            ..Daemon::default()
        }
    }
}

/// Command line options fragment.
///
/// This adds the `-d` (`--daemonize`) and `-f` (`--foreground`) flag to command line. These
/// override whatever is written in configuration (if merged together with the configuration).
///
/// This can be used to transform the [`Daemon`] before daemonization.
///
/// The [`extension`] here can be used to automatically handle both configuration and command line.
/// See the [crate example][index.html#examples].
///
/// Flatten this into the top-level `StructOpt` structure.
///
/// [`extension`]: Daemon::extension
#[derive(Clone, Debug, StructOpt)]
pub struct Opts {
    /// Daemonize ‒ go to background (override the config).
    #[structopt(short = "d", long = "daemonize")]
    daemonize: bool,

    /// Stay in foreground (don't go to background even if config says so).
    #[structopt(short = "f", long = "foreground")]
    foreground: bool,
}

impl Opts {
    /// Returns if daemonization is enabled.
    pub fn daemonize(&self) -> bool {
        self.daemonize && !self.foreground
    }

    /// Modifies the [`daemon`](struct.Daemon.html) according to daemonization set.
    pub fn transform(&self, daemon: Daemon) -> Daemon {
        Daemon {
            daemonize: self.daemonize(),
            ..daemon
        }
    }
}

/// A stripped-down version of [`Daemon`](struct.Daemon.html) without the user-switching options.
///
/// Sometimes, the daemon either needs to keep the root privileges or is started with the
/// appropriate user right away, therefore the user should not be able to configure the `user` and
/// `group` options.
///
/// This configuration fragment serves the role. Convert it to [`Daemon`] first, by
/// [`into_daemon`][UserDaemon::into_daemon] (or using the [`Into`] trait).
///
/// # Examples
///
/// ```rust
/// use spirit_daemonize::{Daemon, UserDaemon};
///
/// // No way to access the `pid_file` and others inside this thing and can't call `.daemonize()`.
/// let user_daemon = UserDaemon::default();
///
/// let daemon: Daemon = user_daemon.into();
/// assert!(daemon.pid_file.is_none());
/// assert_eq!(daemon, Daemon::default());
/// ```
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[serde(rename_all = "kebab-case")]
pub struct UserDaemon {
    /// Where to store a PID file.
    ///
    /// If not set, no PID file is created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pid_file: Option<PathBuf>,

    /// Switch to this working directory at startup.
    ///
    /// If not set, working directory is not switched.
    #[serde(skip_serializing_if = "Option::is_none")]
    workdir: Option<PathBuf>,

    // This is overwritten by [`Opts::transform`](struct.Opts.html#method.transform).
    //
    /// Enable the daemonization.
    ///
    /// Even if this is false, some activity (setting PID file, etc) is still done,
    /// but it doesn't go to background.
    #[serde(default)]
    daemonize: bool,
}

impl UserDaemon {
    /// Converts to full-featured [`Daemon`].
    ///
    /// All the useful functionality is on [`Daemon`], therefore to use it, convert it first.
    ///
    /// It can be also converted using the usual [`From`]/[`Into`] traits.
    pub fn into_daemon(self) -> Daemon {
        self.into()
    }
}
