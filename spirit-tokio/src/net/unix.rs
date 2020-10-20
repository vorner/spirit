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

use std::fmt::Debug;
use std::io::Error as IoError;
use std::os::unix::net::{UnixDatagram as StdUnixDatagram, UnixListener as StdUnixListener};
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

use err_context::prelude::*;
use err_context::AnyError;
use serde::{Deserialize, Serialize};
use spirit::fragment::driver::{CacheSimilar, Comparable, Comparison};
use spirit::fragment::{Fragment, Stackable};
use spirit::Empty;
use tokio::net::{UnixDatagram, UnixListener, UnixStream};

use super::limits::WithLimits;
use super::{Accept, ConfiguredListener};

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
///
/// # TODO
///
/// * Setting permissions on the newly bound socket.
/// * Ability to remove previous socket:
///   - In case it is a dead socket (leftover, but nobody listens on it).
///   - It is any socket.
///   - Always.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(structdoc::StructDoc))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub struct Listen {
    /// The path on the FS where to create the unix domain socket.
    pub path: PathBuf,
    // TODO: Permissions
    // TODO: Remove
}

impl Listen {
    /// Creates a unix listener.
    ///
    /// This is a low-level function, returning the *blocking* (std) listener.
    pub fn create_listener(&self) -> Result<StdUnixListener, AnyError> {
        StdUnixListener::bind(&self.path).map_err(AnyError::from)
    }

    /// Creates a unix datagram socket.
    ///
    /// This is a low-level function, returning the *blocking* (std) socket.
    pub fn create_datagram(&self) -> Result<StdUnixDatagram, AnyError> {
        StdUnixDatagram::bind(&self.path).map_err(AnyError::from)
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
        let mut inc = self.incoming();
        // Unlike the TcpListener, this forces us to go through the incoming :-( But for what it
        // looks inside, this should still work
        Pin::new(&mut inc).poll_accept(ctx)
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
    type Seed = StdUnixListener;
    type Resource = ConfiguredListener<UnixListener, UnixStreamConfig>;
    fn make_seed(&self, name: &str) -> Result<StdUnixListener, AnyError> {
        self.listen
            .create_listener()
            .with_context(|_| format!("Failed to create a unix stream socket {}/{:?}", name, self))
            .map_err(AnyError::from)
    }
    fn make_resource(&self, seed: &mut Self::Seed, name: &str) -> Result<Self::Resource, AnyError> {
        let config = self.unix_config.clone();
        seed.try_clone() // Another copy of the listener
            // std → tokio socket conversion
            .and_then(UnixListener::from_std)
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
    type Seed = StdUnixDatagram;
    type Resource = UnixDatagram;
    fn make_seed(&self, name: &str) -> Result<Self::Seed, AnyError> {
        self.listen
            .create_datagram()
            .with_context(|_| format!("Failed to create unix datagram socket {}/{:?}", name, self))
            .map_err(AnyError::from)
    }
    fn make_resource(&self, seed: &mut Self::Seed, name: &str) -> Result<UnixDatagram, AnyError> {
        seed.try_clone() // Another copy of the socket
            // std → tokio socket conversion
            .and_then(UnixDatagram::from_std)
            .with_context(|_| {
                format!(
                    "Failed to make unix datagram socket {}/{:?} asynchronous",
                    name, self
                )
            })
            .map_err(AnyError::from)
    }
}
