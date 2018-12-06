#![doc(
    html_root_url = "https://docs.rs/spirit-daemonize/0.1.2/spirit_daemonize/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! A spirit helpers and configuration fragments for daemonization.
//!
//! The helpers in here extend the [`spirit`](https://crates.io/crates/spirit) configuration
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
//! use spirit::Spirit;
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
//!    fn daemon(&self) -> Daemon {
//!        self.daemon.clone()
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
//! impl Opts {
//!    fn daemon(&self) -> &DaemonOpts {
//!        &self.daemon
//!    }
//! }
//!
//! fn main() {
//!      Spirit::<Opts, Cfg>::new()
//!         .config_helper(Cfg::daemon, spirit_daemonize::with_opts(Opts::daemon), "daemonize")
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
// For some reason, this produces a warning about unused on nightly… but it is needed on stable
#[allow(unused_imports)]
#[macro_use]
extern crate structopt;

use std::env;
use std::fmt::{Debug, Display};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process;

use failure::{Error, ResultExt};
use nix::sys::stat::{self, Mode};
use nix::unistd::{self, ForkResult, Gid, Uid};
use serde::de::DeserializeOwned;
use spirit::helpers::CfgHelper;
use spirit::validation::Result as ValidationResult;
use spirit::Builder;
use structopt::StructOpt;

/// Configuration of either user or a group.
///
/// This is used to load the configuration into which user and group to drop privileges.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub enum SecId {
    /// Look up based on the name (in `/etc/passwd` or `/etc/group`).
    Name(String),
    /// Don't look up, use this as either uid or gid directly.
    Id(u32),
    /// Don't drop privileges.
    ///
    /// This is not read from configuration, but it is the default value available if nothing is
    /// listed in configuration.
    #[serde(skip)]
    Nothing,
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
/// want to create one, use `Daemon::default` and modify certain fields as needed (this is
/// future-version compatible).
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
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct Daemon {
    /// The user to drop privileges to.
    #[serde(default)]
    pub user: SecId,

    /// The group to drop privileges to.
    #[serde(default)]
    pub group: SecId,

    /// Where to store a PID file.
    ///
    /// If not set, no PID file is created.
    pub pid_file: Option<PathBuf>,

    /// Switch to this working directory at startup.
    ///
    /// If not set, working directory is not switched.
    pub workdir: Option<PathBuf>,

    /// Enable the daemonization.
    ///
    /// Even if this is false, some activity (changing users, setting PID file, etc) is still done.
    ///
    /// This is overwritten by [`Opts::transform`](struct.Opts.html#method.transform).
    #[serde(default)]
    pub daemonize: bool,

    // Prevent the user from creating this directly.
    #[serde(default, skip)]
    sentinel: (),
}

impl Daemon {
    /// Goes into background according to the configuration.
    ///
    /// This does the actual work of daemonization. Usually, this is handled behind the scenes
    /// using the config helper, but if you want you can run it manually.
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
            SecId::Name(ref name) => privdrop::PrivDrop::default().group(&name)?.apply()?,
            SecId::Nothing => (),
        }
        match self.user {
            SecId::Id(id) => unistd::setuid(Uid::from_raw(id))?,
            SecId::Name(ref name) => privdrop::PrivDrop::default().user(&name)?.apply()?,
            SecId::Nothing => (),
        }
        Ok(())
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
/// It can be used either manually, through the [`transform`](struct.transform.html) method or
/// using the [`with_opts`](fn.with_opts.html) function as the second parameter of the
/// `spirit::Builder::config_helper`.
///
/// Flatten this into the top-level `StructOpt` structure.
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
    ///
    /// This is usually used internally from the [`with_opts`](fn.with_opts.html) function.
    pub fn transform(&self, daemon: Daemon) -> Daemon {
        Daemon {
            daemonize: self.daemonize(),
            ..daemon
        }
    }
}

/// Wraps an extractor of an [`Opts`](struct.Opts.html) struct to form a transformation closure for
/// `Daemon` config helper.
///
/// See the [top-level example](index.html#examples).
pub fn with_opts<O, C, F>(mut extractor: F) -> impl FnMut(&O, &C, Daemon) -> Daemon
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    F: FnMut(&O) -> &Opts + Sync + Send + 'static,
{
    move |opts: &O, _: &C, daemon| {
        let opts = extractor(opts);
        opts.transform(daemon)
    }
}

impl<O, C, Transformer> CfgHelper<O, C, Transformer> for Daemon
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    Transformer: FnMut(&O, &C, Daemon) -> Daemon + Sync + Send + 'static,
{
    fn apply<Extractor, Name>(
        mut extractor: Extractor,
        mut transformer: Transformer,
        name: Name,
        builder: Builder<O, C>,
    ) -> Builder<O, C>
    where
        Extractor: FnMut(&C) -> Self + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        let mut previous_daemon = None;
        let validator_hook = move |_: &_, new_cfg: &mut C, opts: &O| -> ValidationResult {
            let daemon = extractor(new_cfg);
            let daemon = transformer(opts, new_cfg, daemon);
            if let Some(previous) = previous_daemon.as_ref() {
                if previous != &daemon {
                    return ValidationResult::warning(format!(
                        "Can't change daemon config {} at runtime for",
                        name,
                    ));
                } else {
                    return ValidationResult::nothing();
                }
            } else if let Err(e) = daemon.daemonize().context("Failed to daemonize") {
                return ValidationResult::from_error(e.into());
            }
            previous_daemon = Some(daemon);
            ValidationResult::nothing()
        };
        builder.config_validator(validator_hook)
    }
}

/// A stripped-down version of [`Daemon`](struct.Daemon.html) without the user-switching options.
///
/// Sometimes, the daemon either needs to keep the root privileges or is started with the
/// appropriate user right away, therefore the user should not be able to configure the `user` and
/// `group` options.
///
/// This configuration fragment serves the role. It can be used as a drop-in replacement with
/// config helpers, but to do anything manually, turn it into `Daemon` first.
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
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct UserDaemon {
    pid_file: Option<PathBuf>,
    workdir: Option<PathBuf>,
    #[serde(default)]
    daemonize: bool,
}

impl<O, C, Transformer> CfgHelper<O, C, Transformer> for UserDaemon
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    Daemon: CfgHelper<O, C, Transformer>,
{
    fn apply<Extractor, Name>(
        mut extractor: Extractor,
        transformer: Transformer,
        name: Name,
        builder: Builder<O, C>,
    ) -> Builder<O, C>
    where
        Extractor: FnMut(&C) -> Self + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        let extractor = move |c: &C| -> Daemon { extractor(c).into() };
        builder.config_helper(extractor, transformer, name)
    }
}
