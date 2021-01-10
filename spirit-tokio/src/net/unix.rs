//! Support for unix domain sockets.
//!
//! This is equivalent to the configuration fragments in the [`net`] module, but for unix domain
//! sockets. This is available only on unix platforms.
//!
//! If you want to accept both normal (IP) sockets and unix domain sockets as part of the
//! configuration, you can use the [`Either`] enum.
//!
//! [`net`]: crate::net
//! [`Either`]: crate::either::Either

use std::cmp;
use std::ffi::OsStr;
use std::fmt::Debug;
use std::fs;
use std::io::Error as IoError;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixDatagram as StdUnixDatagram, UnixListener as StdUnixListener};
use std::path::{Path, PathBuf};
use std::task::{Context, Poll};

use err_context::prelude::*;
use err_context::AnyError;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use socket2::{Domain, SockAddr, Socket, Type as SocketType};
use spirit::fragment::driver::{CacheSimilar, Comparable, Comparison};
use spirit::fragment::{Fragment, Stackable};
use spirit::utils::is_default;
use spirit::{log_error, Empty};
use tokio::net::{UnixDatagram, UnixListener, UnixStream};

use super::limits::WithLimits;
use super::{Accept, ConfiguredListener};

/// When should an existing file/socket be removed before trying to bind to it.
///
/// # Warning
///
/// These settings are racy (they first check the condition and then act on that, but by that time
/// something else might be in place already) and have the possibility to delete unrelated things.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "cfg-help", derive(structdoc::StructDoc))]
pub enum UnlinkBefore {
    /// Try removing it always.
    ///
    /// This has the risk of removing unrelated files.
    Always,

    /// Remove it if it is a socket.
    IsSocket,

    /// Try connecting there first and if connecting fails, remove otherwise keep alive.
    TryConnect,
}

fn is_socket(p: &Path) -> Result<bool, AnyError> {
    let meta = fs::metadata(p)?;
    Ok(meta.file_type().is_socket())
}

fn try_connect(addr: &SockAddr, tp: SocketType) -> Result<bool, AnyError> {
    let socket = Socket::new(Domain::unix(), tp, None)?;
    Ok(socket.connect(addr).is_err())
}

#[doc(hidden)]
pub struct SocketWithPath<S> {
    to_delete: Option<PathBuf>,
    socket: S,
}

impl<S> Drop for SocketWithPath<S> {
    fn drop(&mut self) {
        if let Some(path) = &self.to_delete {
            if let Err(e) = fs::remove_file(path) {
                warn!("Failed to remove {} after use: {}", path.display(), e);
            }
        }
    }
}

/// Configuration of where to bind a unix domain socket.
///
/// This is the lower-level configuration fragment that doesn't directly provide much
/// functionality. But it is the basic building block of both [`UnixListen`] and [`UnixDatagram`].
///
/// Note that this does provide the [`Default`] trait, but the default value is mostly useless.
///
/// # Configuration options
///
/// * `path`: The filesystem path to bind the socket to.
/// * `remove_before`: Under which conditions should the socket be removed before trying to bind
///   it. Beware that this is racy (both because the check is separate from the removal and that
///   something might have been created after we have removed it). No removal by default.
/// * `remove_after`: It an attempt to remove it should be done after we are done using the socket.
///   Default is `false`.
/// * `abstract_path`: If set to `true`, interprets the path as abstract path.
///
/// # TODO
///
/// * Setting permissions on the newly bound socket.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(structdoc::StructDoc))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub struct Listen {
    /// The path on the FS where to create the unix domain socket.
    pub path: PathBuf,

    /// When should an existing file/socket be removed before trying to bind to it.
    ///
    /// # Warning
    ///
    /// These settings are racy (they first check the condition and then act on that, but by that time
    /// something else might be in place already) and have the possibility to delete unrelated things.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unlink_before: Option<UnlinkBefore>,

    /// Try removing the file after stopping using it.
    ///
    /// Removal might fail in case of unclean shutdown and be left behind.
    #[serde(default, skip_serializing_if = "is_default")]
    pub unlink_after: bool,

    /// Interpret as an abstract path instead of ordinary one.
    #[serde(default, rename = "abstract", skip_serializing_if = "is_default")]
    pub abstract_path: bool,

    /// The accepting backlog.
    ///
    /// Has no effect for DGRAM sockets.
    ///
    /// This specifies how many connections can wait in the kernel before being accepted. If more
    /// than this limit are queued, the kernel starts refusing them with connection reset packets.
    ///
    /// The default is 128.
    #[serde(default = "super::default_backlog")]
    pub backlog: u32,
    // TODO: Permissions
}

impl Listen {
    fn add_path<S>(&self, socket: S) -> SocketWithPath<S> {
        let to_delete = if self.unlink_after && !self.abstract_path {
            Some(self.path.clone())
        } else {
            None
        };
        SocketWithPath { socket, to_delete }
    }
    fn unlink_before(&self, addr: &SockAddr, tp: SocketType) {
        use UnlinkBefore::*;

        let unlink = match (self.abstract_path, self.unlink_before) {
            (true, _) | (false, None) => false,
            (false, Some(Always)) => true,
            (false, Some(IsSocket)) => is_socket(&self.path).unwrap_or(false),
            (false, Some(TryConnect)) => {
                self.path.exists() && try_connect(addr, tp).unwrap_or(false)
            }
        };

        if unlink {
            if let Err(e) = fs::remove_file(&self.path) {
                log_error!(
                    Warn,
                    e.context(format!(
                        "Failed to remove previous socket at {}",
                        self.path.display()
                    ))
                    .into()
                );
            } else {
                info!("Removed previous socket {}", self.path.display());
            }
        }
    }

    fn create_any(&self, tp: SocketType) -> Result<Socket, AnyError> {
        let mut buf = Vec::new();
        let addr: &OsStr = if self.abstract_path {
            buf.push(0);
            buf.extend(self.path.as_os_str().as_bytes());
            OsStr::from_bytes(&buf)
        } else {
            self.path.as_ref()
        };
        let addr = SockAddr::unix(addr).context("Create sockaddr")?;
        let sock = Socket::new(Domain::unix(), tp, None).context("Create socket")?;
        self.unlink_before(&addr, tp);
        sock.bind(&addr)
            .with_context(|_| format!("Binding socket to {}", self.path.display()))?;

        Ok(sock)
    }

    /// Creates a unix listener.
    ///
    /// This is a low-level function, returning the *blocking* (std) listener.
    pub fn create_listener(&self) -> Result<StdUnixListener, AnyError> {
        let sock = self.create_any(SocketType::stream())?;
        sock.listen(cmp::min(self.backlog, i32::max_value() as u32) as i32)
            .context("Listening to Stream socket")?;
        Ok(sock.into_unix_listener())
    }

    /// Creates a unix datagram socket.
    ///
    /// This is a low-level function, returning the *blocking* (std) socket.
    pub fn create_datagram(&self) -> Result<StdUnixDatagram, AnyError> {
        let sock = self.create_any(SocketType::dgram())?;
        Ok(sock.into_unix_datagram())
    }
}

/// Additional configuration for unix domain stream sockets.
///
/// *Currently* this is an alias to `Empty`, because there haven't been yet any idea what further
/// to configure on them. However, this can turn into its own type in some future time and it won't
/// be considered semver-incompatible change.
///
/// If you want to always have no additional configuration, use [`Empty`] explicitly.
pub type UnixConfig = Empty;

impl Accept for UnixListener {
    type Connection = UnixStream;
    fn poll_accept(&mut self, ctx: &mut Context) -> Poll<Result<Self::Connection, IoError>> {
        mod inner {
            use super::{Context, IoError, Poll, UnixListener, UnixStream};
            // Hide the Accept trait from scope so it doesn't interfere
            pub(super) fn poll_accept(
                l: &UnixListener,
                ctx: &mut Context,
            ) -> Poll<Result<UnixStream, IoError>> {
                l.poll_accept(ctx).map_ok(|(s, _)| s)
            }
        }
        inner::poll_accept(self, ctx)
    }
}

/// A listener for unix domain stream sockets.
///
/// This is the unix-domain equivalent of [`TcpListen`]. All notes about it apply here with the
/// sole difference that the fields added by it are from [`unix::Listen`] instead of
/// [`net::Listen`].
///
/// [`TcpListen`]: crate::net::TcpListen
/// [`unix::Listen`]: Listen
/// [`net::Listen`]: crate::net::Listen
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(structdoc::StructDoc))]
pub struct UnixListen<ExtraCfg = Empty, UnixStreamConfig = UnixConfig> {
    #[serde(flatten)]
    listen: Listen,
    #[serde(flatten)]
    unix_config: UnixStreamConfig,

    /// Arbitrary additional application-specific configuration that doesn't influence the socket.
    ///
    /// But it can be looked into by the [`handlers`][crate::handlers].
    #[serde(flatten)]
    pub extra_cfg: ExtraCfg,
}

impl<ExtraCfg, UnixStreamConfig> Stackable for UnixListen<ExtraCfg, UnixStreamConfig> {}

impl<ExtraCfg, UnixStreamConfig> Comparable for UnixListen<ExtraCfg, UnixStreamConfig>
where
    ExtraCfg: PartialEq,
    UnixStreamConfig: PartialEq,
{
    fn compare(&self, other: &Self) -> Comparison {
        if self.listen != other.listen {
            Comparison::Dissimilar
        } else if self != other {
            Comparison::Similar
        } else {
            Comparison::Same
        }
    }
}

impl<ExtraCfg, UnixStreamConfig> Fragment for UnixListen<ExtraCfg, UnixStreamConfig>
where
    ExtraCfg: Clone + Debug + PartialEq,
    UnixStreamConfig: Clone + Debug + PartialEq,
{
    type Driver = CacheSimilar<Self>;
    type Installer = ();
    type Seed = SocketWithPath<StdUnixListener>;
    type Resource = ConfiguredListener<UnixListener, UnixStreamConfig>;
    fn make_seed(&self, name: &str) -> Result<Self::Seed, AnyError> {
        self.listen
            .create_listener()
            .with_context(|_| format!("Failed to create a unix stream socket {}/{:?}", name, self))
            .map_err(AnyError::from)
            .map(|s| self.listen.add_path(s))
    }
    fn make_resource(&self, seed: &mut Self::Seed, name: &str) -> Result<Self::Resource, AnyError> {
        let config = self.unix_config.clone();
        seed.socket
            .try_clone() // Another copy of the listener
            // std → tokio socket conversion
            .and_then(|sock| -> Result<UnixListener, IoError> {
                sock.set_nonblocking(true)?;
                UnixListener::from_std(sock)
            })
            .with_context(|_| {
                format!(
                    "Failed to make unix streamsocket {}/{:?} asynchronous",
                    name, self
                )
            })
            .map_err(AnyError::from)
            .map(|listener| ConfiguredListener::new(listener, config))
    }
}

/// Type alias for [`UnixListen`] without any unnecessary configuration options.
pub type MinimalUnixListen<ExtraCfg = Empty> = UnixListen<ExtraCfg, Empty>;

/// Wrapped [`UnixListen`] that limits the number of concurrent connections and handles some
/// recoverable errors.
pub type UnixListenWithLimits<ExtraCfg = Empty, UnixStreamConfig = UnixConfig> =
    WithLimits<UnixListen<ExtraCfg, UnixStreamConfig>>;

/// A [`Fragment`] for unix datagram sockets
///
/// This is an unix domain equivalent to the [`UdpListen`] configuration fragment. All its notes
/// apply here except that the base configuration fields are taken from [`unix::Listen`] instead of
/// [`net::Listen`].
///
/// [`UdpListen`]: crate::net::UdpListen
/// [`unix::Listen`]: Listen
/// [`net::Listen`]: crate::net::Listen
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(structdoc::StructDoc))]
#[non_exhaustive]
pub struct DatagramListen<ExtraCfg = Empty> {
    /// The listening address.
    #[serde(flatten)]
    pub listen: Listen,

    /// Arbitrary application-specific configuration that doesn't influence the socket itself.
    ///
    /// But it can be examined from within the [`handlers`][crate::handlers].
    #[serde(flatten)]
    pub extra_cfg: ExtraCfg,
}

impl<ExtraCfg> Stackable for DatagramListen<ExtraCfg> {}

impl<ExtraCfg: PartialEq> Comparable for DatagramListen<ExtraCfg> {
    fn compare(&self, other: &Self) -> Comparison {
        if self.listen != other.listen {
            Comparison::Dissimilar
        } else if self != other {
            Comparison::Similar
        } else {
            Comparison::Same
        }
    }
}

impl<ExtraCfg> Fragment for DatagramListen<ExtraCfg>
where
    ExtraCfg: Clone + Debug + PartialEq,
{
    type Driver = CacheSimilar<Self>;
    type Installer = ();
    type Seed = SocketWithPath<StdUnixDatagram>;
    type Resource = UnixDatagram;
    fn make_seed(&self, name: &str) -> Result<Self::Seed, AnyError> {
        self.listen
            .create_datagram()
            .with_context(|_| format!("Failed to create unix datagram socket {}/{:?}", name, self))
            .map_err(AnyError::from)
            .map(|s| self.listen.add_path(s))
    }
    fn make_resource(&self, seed: &mut Self::Seed, name: &str) -> Result<UnixDatagram, AnyError> {
        seed.socket
            .try_clone() // Another copy of the socket
            // std → tokio socket conversion
            .and_then(|sock| -> Result<UnixDatagram, IoError> {
                sock.set_nonblocking(true)?;
                UnixDatagram::from_std(sock)
            })
            .with_context(|_| {
                format!(
                    "Failed to make unix datagram socket {}/{:?} asynchronous",
                    name, self
                )
            })
            .map_err(AnyError::from)
    }
}
