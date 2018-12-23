//! Support for unix domain sockets.
//!
//! This is equivalent to the configuration fragments in the [`net`] module, but for unix domain
//! sockets. This is available only on unix platforms.
//!
//! If you want to accept both normal (IP) sockets and unix domain sockets as part of the
//! configuration, you can use the [`Either`] enum.
//!
//! [`net`]: ::net
//! [`Either`]: ::either::Either

use std::fmt::Debug;
use std::os::unix::net::{UnixDatagram as StdUnixDatagram, UnixListener as StdUnixListener};
use std::path::PathBuf;

use failure::{Error, ResultExt};
use spirit::validation::Results as ValidationResults;
use spirit::Empty;
use tokio::net::unix::{Incoming, UnixDatagram, UnixListener, UnixStream};
use tokio::reactor::Handle;

use base_traits::ResourceConfig;
use net::limits::WithListenLimits;
use net::{ConfiguredStreamListener, IntoIncoming, StreamConfig};
use scaled::{Scale, Scaled};

/// Configuration of where to bind a unix domain socket.
///
/// This is the lower-level configuration fragment that doesn't directly provide much
/// functionality. But it is the basic building block of both [`UnixListen`] and [`UnixDatagram`].
///
/// Note that this does provide the [`Default`] trait, but the default value is mostly useless.
///
/// # Fragmets
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
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, StructDoc)]
#[serde(rename_all = "kebab-case")]
pub struct Listen {
    path: PathBuf,
    // TODO: Permissions
    // TODO: Remove
}

impl Listen {
    /// Creates a unix listener.
    ///
    /// This is a low-level function, returning the *blocking* (std) listener.
    pub fn create_listener(&self) -> Result<StdUnixListener, Error> {
        StdUnixListener::bind(&self.path).map_err(Error::from)
    }

    /// Creates a unix datagram socket.
    ///
    /// This is a low-level function, returning the *blocking* (std) socket.
    pub fn create_datagram(&self) -> Result<StdUnixDatagram, Error> {
        StdUnixDatagram::bind(&self.path).map_err(Error::from)
    }
}

/// Additional config for unix domain stream sockets.
///
/// *Currently* this is an alias to `Empty`, because there haven't been yet any idea what further
/// to configure on them. However, this can turn into its own type in some future time and it won't
/// be considered semver-incompatible change.
///
/// If you want to always have no additional configuration, use [`Empty`] explicitly.
pub type UnixConfig = Empty;

impl IntoIncoming for UnixListener {
    type Connection = UnixStream;
    type Incoming = Incoming;
    fn into_incoming(self) -> Incoming {
        self.incoming()
    }
}

/// A listener for unix domain stream sockets.
///
/// This is the unix-domain equivalent of [`TcpListen`]. All notes about it apply here with the
/// sole difference that the fields added by it are from [`unix::Listen`] instead of
/// [`net::Listen`].
///
/// [`TcpListen`]: ::net::TcpListen
/// [`unix::Listen`]: Listen
/// [`net::Listen`]: ::net::Listen
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, StructDoc)]
pub struct UnixListen<ExtraCfg = Empty, ScaleMode = Scale, UnixStreamConfig = UnixConfig> {
    #[serde(flatten)]
    listen: Listen,
    #[serde(flatten)]
    scale: ScaleMode,
    #[serde(flatten)]
    extra_cfg: ExtraCfg,
    #[serde(flatten)]
    unix_config: UnixStreamConfig,
}

impl<ExtraCfg, ScaleMode, UnixStreamConfig, O, C> ResourceConfig<O, C>
    for UnixListen<ExtraCfg, ScaleMode, UnixStreamConfig>
where
    ExtraCfg: Debug + PartialEq + Send + Sync + 'static,
    ScaleMode: Debug + PartialEq + Scaled + Send + Sync + 'static,
    UnixStreamConfig: Clone + Debug + PartialEq + Send + Sync + StreamConfig<UnixStream> + 'static,
{
    type Seed = StdUnixListener;
    type Resource = ConfiguredStreamListener<UnixListener, UnixStreamConfig>;
    fn create(&self, name: &str) -> Result<StdUnixListener, Error> {
        self.listen
            .create_listener()
            .with_context(|_| format!("Failed to create socket {}/{:?}", name, self))
            .map_err(Error::from)
    }
    fn fork(&self, seed: &StdUnixListener, name: &str) -> Result<Self::Resource, Error> {
        let config = self.unix_config.clone();
        seed.try_clone() // Another copy of the listener
            // std → tokio socket conversion
            .and_then(|listener| UnixListener::from_std(listener, &Handle::default()))
            .with_context(|_| format!("Failed to fork socket {}/{:?}", name, self))
            .map_err(Error::from)
            .map(|listener| ConfiguredStreamListener::new(listener, config))
    }
    fn scaled(&self, name: &str) -> (usize, ValidationResults) {
        self.scale.scaled(name)
    }
    fn is_similar(&self, other: &Self, _: &str) -> bool {
        self.listen == other.listen
    }
}

/// Type alias for [`UnixListen`] without any unnecessary configuration options.
pub type MinimalUnixListen<ExtraCfg = Empty> = UnixListen<ExtraCfg, Empty, Empty>;

/// Wrapped [`UnixListen`] for use with the [`per_connection`] [`ResourceConsumer`]
///
/// [`ResourceConsumer`]: ::ResourceConsumer
pub type UnixListenWithLimits<ExtraCfg = Empty, ScaleMode = Scale, UnixStreamConfig = UnixConfig> =
    WithListenLimits<UnixListen<ExtraCfg, ScaleMode, UnixStreamConfig>>;

/// A [`ResourceConfig`] for unix datagram sockets
///
/// This is an unix domain equivalent to the [`UdpListen`] configuration fragment. All its notes
/// apply here except that the base configuration fields are taken from [`unix::Listen`] instead of
/// [`net::Listen`].
///
/// [`UdpListen`]: ::UdpListen
/// [`unix::Listen`]: Listen
/// [`net::Listen`]: ::net::Listen
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, StructDoc)]
pub struct DatagramListen<ExtraCfg = Empty, ScaleMode = Scale> {
    #[serde(flatten)]
    listen: Listen,
    #[serde(flatten)]
    scale: ScaleMode,
    #[serde(flatten)]
    extra_cfg: ExtraCfg,
}

impl<ExtraCfg, ScaleMode, O, C> ResourceConfig<O, C> for DatagramListen<ExtraCfg, ScaleMode>
where
    ExtraCfg: Debug + PartialEq + Send + Sync + 'static,
    ScaleMode: Debug + PartialEq + Scaled + Send + Sync + 'static,
{
    type Seed = StdUnixDatagram;
    type Resource = UnixDatagram;
    fn create(&self, name: &str) -> Result<StdUnixDatagram, Error> {
        self.listen
            .create_datagram()
            .with_context(|_| format!("Failed to create socket {}/{:?}", name, self))
            .map_err(Error::from)
    }
    fn fork(&self, seed: &StdUnixDatagram, name: &str) -> Result<UnixDatagram, Error> {
        seed.try_clone() // Another copy of the socket
            // std → tokio socket conversion
            .and_then(|socket| UnixDatagram::from_std(socket, &Handle::default()))
            .with_context(|_| format!("Failed to fork socket {}/{:?}", name, self))
            .map_err(Error::from)
    }
    fn scaled(&self, name: &str) -> (usize, ValidationResults) {
        self.scale.scaled(name)
    }
    fn is_similar(&self, other: &Self, _: &str) -> bool {
        self.listen == other.listen
    }
}

extra_cfg_impl! {
    UnixListen<ExtraCfg, ScaleMode, UnixStreamConfig>::extra_cfg: ExtraCfg;
    DatagramListen<ExtraCfg, ScaleMode>::extra_cfg: ExtraCfg;
}

cfg_helpers! {
    impl helpers for UnixListen <ExtraCfg, ScaleMode, UnixStreamConfig>
    where
        ExtraCfg: Debug + PartialEq + Send + Sync + 'static,
        ScaleMode: Debug + PartialEq + Scaled + Send + Sync + 'static,
        UnixStreamConfig: Debug + PartialEq + Send + Sync + 'static;

    impl helpers for DatagramListen <ExtraCfg, ScaleMode>
    where
        ExtraCfg: Debug + PartialEq + Send + Sync + 'static,
        ScaleMode: Debug + PartialEq + Scaled + Send + Sync + 'static;
}
