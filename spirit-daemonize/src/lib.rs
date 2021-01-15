#![doc(test(attr(deny(warnings))))]
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
//! use spirit::Spirit;
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
//!         .with(unsafe {
//!             spirit_daemonize::extension(|c: &Cfg, o: &Opts| {
//!                 (c.daemon.clone(), o.daemon.clone())
//!             })
//!         })
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
use std::sync::Arc;

use err_context::prelude::*;
use log::{debug, trace, warn};
use nix::sys::stat::{self, Mode};
use nix::unistd::{self, ForkResult, Gid, Uid};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use spirit::extension::{Extensible, Extension};
use spirit::validation::Action;

use spirit::AnyError;
use structdoc::StructDoc;
#[cfg(feature = "cfg-help")]
use structopt::StructOpt;

/// Intermediate plumbing type.
///
/// This is passed through the [`Pipeline`][spirit::Pipeline] as a way of signalling the next
/// actions. Users are not expected to interact directly with this.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub struct Daemonize {
    /// Go into background.
    pub daemonize: bool,

    /// Store our own PID into this file.
    ///
    /// The one after we double-fork, so it is the real daemon's one.
    pub pid_file: Option<PathBuf>,
}

impl Daemonize {
    /// Goes into background according to the configuration.
    ///
    /// This does the actual work of daemonization. This can be used manually.
    ///
    /// This is not expected to fail in practice, as all checks are performed in advance. It can
    /// fail for rare race conditions (eg. the directory where the PID file should go disappears
    /// between the check and now) or if there are not enough PIDs available to fork.
    ///
    /// # Safety
    ///
    /// This is safe to call only if either the application is yet single-threaded (it's OK to
    /// start more threads afterwards) or if `daemonize` is `false`.
    pub unsafe fn daemonize(&self) -> Result<(), AnyError> {
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
    /// [`Daemonize`]. This is mostly used through [`extension'], but can also be used manually.
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
    /// management and [`extension`].
    ///
    /// # Safety
    ///
    /// This is safe to call only if either the application is yet single-threaded (it's OK to
    /// start more threads afterwards) or if `daemonize` is `false`.
    pub unsafe fn daemonize(&self) -> Result<(), AnyError> {
        self.prepare()?.daemonize()?;
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

impl From<(Daemon, Opts)> for Daemon {
    fn from((d, o): (Daemon, Opts)) -> Self {
        o.transform(d)
    }
}

impl From<(UserDaemon, Opts)> for Daemon {
    fn from((d, o): (UserDaemon, Opts)) -> Self {
        o.transform(d.into())
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

See the [`extension`] for how to use it.
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

/// Creates an extension for daemonization as part of the spirit.
///
/// Goes into background (or not) as configured by configuration and options (see the crate
/// example).
///
/// # Safety
///
/// Daemonization uses `fork`. Therefore, the caller must ensure this is run only before any
/// threads are started.
///
/// This runs in the validation phase of the config validator.
///
/// Note that many thing may start threads. Specifically, logging with a background thread and the
/// Tokio runtime do.
///
/// If you want to have logging available before daemonization (to have some output during/after
/// daemonization), it is possible to do a two-phase initialization ‒ once before daemonization,
/// but without a background thread, and then after. See the [relevant chapter in the
/// guide][spirit::guide::daemonization].
pub unsafe fn extension<E, C, D>(extractor: C) -> impl Extension<E>
where
    E: Extensible<Ok = E>,
    E::Config: DeserializeOwned + Send + Sync + 'static,
    E::Opts: StructOpt + Send + Sync + 'static,
    C: Fn(&E::Config, &E::Opts) -> D + Send + Sync + 'static,
    D: Into<Daemon>,
{
    move |e: E| {
        let init = move |_: &_, cfg: &Arc<_>, opts: &_| -> Result<Action, AnyError> {
            let d = extractor(&cfg, opts);
            d.into().daemonize()?;
            Ok(Action::new())
        };
        e.config_validator(init)
    }
}
