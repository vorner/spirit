#![doc(test(attr(deny(warnings))))]
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
//! use serde::Deserialize;
//! use spirit::{Pipeline, Spirit};
//! use spirit::prelude::*;
//! use spirit_daemonize::{Daemon, Opts as DaemonOpts};
//! use structopt::StructOpt;
//!
//! // From config files
//! #[derive(Default, Deserialize)]
//! struct Cfg {
//!     #[serde(default)]
//!     daemon: Daemon,
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
//!         .with(
//!             Pipeline::new("daemon")
//!                 .extract(|o: &Opts, c: &Cfg| {
//!                     // Put the two sources together to one Daemon
//!                     o.daemon.transform(c.daemon.clone())
//!                 })
//!         )
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
//! The daemonization happens inside the application of [validator
//! actions][spirit::validation::Action] of `config_validator` callback. If other config validators
//! need to start any threads, they should be plugged in after the daemonization callback. However,
//! the safer option is to start them inside the `run` method.

use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process;

use err_context::prelude::*;
use log::{debug, trace, warn};
use nix::sys::stat::{self, Mode};
use nix::unistd::{self, ForkResult, Gid, Uid};
use serde::{Deserialize, Serialize};
use spirit::error::log_errors;
use spirit::fragment::driver::OnceDriver;
use spirit::fragment::Installer;

use spirit::AnyError;
use structdoc::StructDoc;
#[cfg(feature = "cfg-help")]
use structopt::StructOpt;

/// Intermediate plumbing type.
///
/// This is passed through the [`Pipeline`][spirit::Pipeline] as a way of signalling the next
/// actions. Users are not expected to interact directly with this.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Daemonize {
    daemonize: bool,
    pid_file: Option<PathBuf>,
}

impl Daemonize {
    /// Goes into background according to the configuration.
    ///
    /// This does the actual work of daemonization. This can be used manually.
    ///
    /// This is not expected to fail in practice, as all checks are performed in advance. It can
    /// fail for rare race conditions (eg. the directory where the PID file should go disappears
    /// between the check and now) or if there are not enough PIDs available to fork.
    pub fn daemonize(&self) -> Result<(), AnyError> {
        if self.daemonize {
            trace!("Redirecting stdio");
            let devnull = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open("/dev/null")
                .context("Failed to open /dev/null")?;
            for fd in &[0, 1, 2] {
                unistd::dup2(devnull.as_raw_fd(), *fd)
                    .with_context(|_| format!("Failed to redirect FD {}", fd))?;
            }
            trace!("Doing double fork");
            if let ForkResult::Parent { .. } = unistd::fork().context("Failed to fork")? {
                process::exit(0);
            }
            unistd::setsid()?;
            if let ForkResult::Parent { .. } = unistd::fork().context("Failed to fork")? {
                process::exit(0);
            }
        } else {
            trace!("Not going to background");
        }
        if let Some(file) = self.pid_file.as_ref() {
            let mut f = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o644)
                .open(file)
                .with_context(|_| format!("Failed to write PID file {}", file.display()))?;
            writeln!(f, "{}", unistd::getpid())?;
        }
        Ok(())
    }
}

/// Configuration of either user or a group.
///
/// This is used to load the configuration into which user and group to drop privileges.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[serde(untagged)]
#[non_exhaustive]
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
#[non_exhaustive]
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
}

impl Daemon {
    /// Prepare for daemonization.
    ///
    /// This does the fist (fallible) phase of daemonization and schedules the rest, though the
    /// [`Daemonize`]. This is mostly used through [`spirit::Pipeline`] (see the [`crate`]
    /// examples), if used outside of them, prefer the [`daemonize`][Daemon::daemonize].
    pub fn prepare(&self) -> Result<Daemonize, AnyError> {
        // TODO: Tests for this
        debug!("Preparing to daemonize with {:?}", self);

        stat::umask(Mode::empty()); // No restrictions on write modes
        let workdir = self
            .workdir
            .as_ref()
            .map(|pb| pb as &Path)
            .unwrap_or_else(|| Path::new("/"));
        trace!("Changing working directory to {:?}", workdir);
        env::set_current_dir(workdir)
            .with_context(|_| format!("Failed to switch to workdir {}", workdir.display()))?;

        // PrivDrop implements the necessary libc lookups to find the group and
        //  user entries matching the given names. If these queries fail,
        //  because the user or group names are invalid, the function will fail.
        match self.group {
            SecId::Id(id) => {
                unistd::setgid(Gid::from_raw(id)).context("Failed to change the group")?
            }
            SecId::Name(ref name) => privdrop::PrivDrop::default()
                .group(&name)
                .apply()
                .context("Failed to change the group")?,
            SecId::Nothing => (),
        }
        match self.user {
            SecId::Id(id) => {
                unistd::setuid(Uid::from_raw(id)).context("Failed to change the user")?
            }
            SecId::Name(ref name) => privdrop::PrivDrop::default()
                .user(&name)
                .apply()
                .context("Failed to change the user")?,
            SecId::Nothing => (),
        }

        // We try to check the PID file is writable, as we still can reasonably report errors now.
        if let Some(file) = self.pid_file.as_ref() {
            let _ = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o644)
                .open(file)
                .with_context(|_| format!("Writing the PID file {}", file.display()))?;
        }

        Ok(Daemonize {
            daemonize: self.daemonize,
            pid_file: self.pid_file.clone(),
        })
    }

    /// Perform the daemonization according to the setup inside.
    ///
    /// This is the routine one would call if using the fragments manually, not through the spirit
    /// management and [`Pipeline`][spirit::Pipeline]s.
    pub fn daemonize(&self) -> Result<(), AnyError> {
        self.prepare()?.daemonize()?;
        Ok(())
    }
}

/// An installer for [`Daemonize`].
///
/// Mostly internal plumbing type, used through [`Pipeline`][spirit::Pipeline].
///
/// If the daemonization fails, the program aborts with a logged error.
#[derive(Copy, Clone, Debug, Default)]
pub struct DaemonizeInstaller;

impl<O, C> Installer<Daemonize, O, C> for DaemonizeInstaller {
    type UninstallHandle = ();
    fn install(&mut self, daemonize: Daemonize, _: &str) {
        // If we can't daemonize, we abort it. It's really unlikely to happen, though.
        if log_errors(module_path!(), || daemonize.daemonize()).is_err() {
            // The error has already been logged
            process::abort();
        }
    }
}

spirit::simple_fragment! {
    impl Fragment for Daemon {
        type Driver = OnceDriver<Self>;
        type Resource = Daemonize;
        type Installer = DaemonizeInstaller;
        fn create(&self, _: &'static str) -> Result<Daemonize, AnyError> {
            self.prepare()
        }
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

// Workaround for https://github.com/TeXitoi/structopt/issues/333
#[cfg_attr(not(doc), allow(missing_docs))]
#[cfg_attr(
    doc,
    doc = r#"
Command line options fragment.

This adds the `-d` (`--daemonize`) and `-f` (`--foreground`) flag to command line. These
override whatever is written in configuration (if merged together with the configuration).

This can be used to transform the [`Daemon`] before daemonization.

The [`Pipeline`] here can be used to automatically handle both configuration and command line.
See the [crate example][index.html#examples].

Flatten this into the top-level `StructOpt` structure.

[`Pipeline`]: spirit::Pipeline
"#
)]
#[derive(Clone, Debug, StructOpt)]
#[non_exhaustive]
pub struct Opts {
    /// Daemonize ‒ go to background (override the config).
    #[structopt(short, long)]
    pub daemonize: bool,

    /// Stay in foreground (don't go to background even if config says so).
    #[structopt(short, long)]
    pub foreground: bool,
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
#[non_exhaustive]
pub struct UserDaemon {
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
    /// Even if this is false, some activity (setting PID file, etc) is still done,
    /// but it doesn't go to background.
    #[serde(default)]
    pub daemonize: bool,
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

spirit::simple_fragment! {
    impl Fragment for UserDaemon {
        type Driver = OnceDriver<Self>;
        type Resource = Daemonize;
        type Installer = DaemonizeInstaller;
        fn create(&self, _: &'static str) -> Result<Daemonize, AnyError> {
            Daemon::from(self.clone()).prepare()
        }
    }
}
