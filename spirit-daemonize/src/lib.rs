#![doc(
    html_root_url = "https://docs.rs/spirit-daemonize/0.1.0/spirit-daemonize/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
// #![warn(missing_docs)] TODO

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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub enum SecId {
    Name(String),
    Id(u32),
    #[serde(skip)]
    Nothing,
}

impl Default for SecId {
    fn default() -> Self {
        SecId::Nothing
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct Daemon {
    #[serde(default)]
    pub user: SecId,
    #[serde(default)]
    pub group: SecId,
    pub pid_file: Option<PathBuf>,
    pub workdir: Option<PathBuf>,
    #[serde(default, skip)]
    pub daemonize: bool,
    #[serde(default, skip)]
    sentinel: (),
}

impl Daemon {
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

    pub fn enabled(self) -> Daemon {
        Daemon {
            daemonize: true,
            ..self
        }
    }
}

#[derive(Clone, Debug, StructOpt)]
pub struct DaemonOpts {
    /// Daemonize ‒ go to background.
    #[structopt(short = "d", long = "daemonize")]
    daemonize: bool,
}

impl DaemonOpts {
    pub fn daemonize(&self) -> bool {
        self.daemonize
    }
    pub fn transform(&self, daemon: Daemon) -> Daemon {
        Daemon {
            daemonize: self.daemonize(),
            ..daemon
        }
    }
}

pub fn with_opts<O, C, F>(mut extractor: F) -> impl FnMut(&O, &C, Daemon) -> Daemon
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    F: FnMut(&O) -> &DaemonOpts + Sync + Send + 'static,
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
