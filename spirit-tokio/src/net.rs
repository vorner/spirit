use std::fmt::Debug;
use std::io::Error as IoError;
use std::net::{TcpListener as StdTcpListener, UdpSocket as StdUdpSocket};
use std::time::Duration;

use failure::Error;
use futures::Stream;
use serde_humanize_rs;
use spirit::validation::Results as ValidationResults;
use spirit::Empty;
use tokio::net::tcp::Incoming;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::reactor::Handle;

use super::{ExtraCfgCarrier, ResourceConfig, ResourceConsumer};
use scaled::{Scale, Scaled};

pub trait IntoIncoming: Send + Sync + 'static {
    type Connection: Send + Sync + 'static;
    type Incoming: Stream<Item = Self::Connection, Error = IoError> + Send + Sync + 'static;
    fn into_incoming(self) -> Self::Incoming;
}

impl IntoIncoming for TcpListener {
    type Connection = TcpStream;
    type Incoming = Incoming;
    fn into_incoming(self) -> Self::Incoming {
        self.incoming()
    }
}

pub trait ListenLimits {
    fn error_sleep(&self) -> Duration;
    fn max_conn(&self) -> usize;
}

fn default_host() -> String {
    "::".to_owned()
}

/// A description of listening interface and port.
///
/// This can be used as part of configuration to describe a socket.
///
/// Note that the `Default` implementation is there to provide something on creation of `Spirit`,
/// but isn't very useful.
///
/// It contains these configuration options:
///
/// * `port` (mandatory)
/// * `host` (optional, if not present, `*` is used)
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Listen {
    port: u16,
    #[serde(default = "default_host")]
    host: String,
}

impl Default for Listen {
    fn default() -> Self {
        Listen {
            port: 0,
            host: default_host(),
        }
    }
}

impl Listen {
    /// Creates a TCP socket described by the loaded configuration.
    pub fn create_tcp(&self) -> Result<StdTcpListener, Error> {
        Ok(StdTcpListener::bind((&self.host as &str, self.port))?)
    }
    /// Creates a UDP socket described by the loaded configuration.
    pub fn create_udp(&self) -> Result<StdUdpSocket, Error> {
        Ok(StdUdpSocket::bind((&self.host as &str, self.port))?)
    }
}

/// A configuration fragment of a TCP listening socket.
///
/// This describes a TCP listening socket. It works as an iterated configuration helper ‒ if you
/// extract a vector of these from configuration and an action to handle one TCP connection, it
/// manages the listening itself when attached to spirit.
///
/// # Type parameters
///
/// * `ExtraCfg`: Any additional configuration options, passed to the action callback. Defaults to
///   an empty set of parameters.
/// * `ScaleMode`: A description of how to scale into multiple listening instances.
///
/// # Configuration options
///
/// Aside from the options from the type parameters above, these options are present:
///
/// * `host`: The host/interface to listen on, defaults to `*`.
/// * `port`: Mandatory, the port to listen to.
/// * `error-sleep`: If there's a recoverable error like „Too many open files“, sleep this long
///   before trying again to accept more connections. Defaults to "100ms".
/// * `max-conn`: Maximum number of parallel connections. This is per one instance, therefore the
///   total number of connections being handled is `scale * max_conn` (if scaling is enabled).
///   Defaults to 1000.
///
/// TODO: The max-conn is in the WithListenLimits
///
/// # Example
///
/// TODO (adjust the one from the crate level config)
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TcpListen<ExtraCfg = Empty, ScaleMode: Scaled = Scale> {
    #[serde(flatten)]
    listen: Listen,
    #[serde(flatten)]
    scale: ScaleMode,
    #[serde(flatten)]
    extra_cfg: ExtraCfg,
}

impl<ExtraCfg, ScaleMode> ExtraCfgCarrier for TcpListen<ExtraCfg, ScaleMode>
where
    ScaleMode: Scaled,
{
    type Extra = ExtraCfg;
    fn extra(&self) -> &ExtraCfg {
        &self.extra_cfg
    }
}

impl<ExtraCfg, ScaleMode, O, C> ResourceConfig<O, C> for TcpListen<ExtraCfg, ScaleMode>
where
    ExtraCfg: Debug + PartialEq + Send + Sync + 'static,
    ScaleMode: Debug + PartialEq + Scaled + Send + Sync + 'static,
{
    type Seed = StdTcpListener;
    type Resource = TcpListener;
    fn create(&self, _: &str) -> Result<StdTcpListener, Error> {
        self.listen.create_tcp()
    }
    fn fork(&self, seed: &StdTcpListener, _: &str) -> Result<TcpListener, Error> {
        seed.try_clone() // Another copy of the listener
            // std → tokio socket conversion
            .and_then(|listener| TcpListener::from_std(listener, &Handle::default()))
            .map_err(Error::from)
    }
    fn scaled(&self, name: &str) -> (usize, ValidationResults) {
        self.scale.scaled(name)
    }
    fn is_similar(&self, other: &Self, _: &str) -> bool {
        self.listen == other.listen
    }
}

fn default_error_sleep() -> Duration {
    Duration::from_millis(100)
}

fn default_max_conn() -> usize {
    1000
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct WithListenLimits<Listener> {
    #[serde(flatten)]
    inner: Listener,
    #[serde(
        rename = "error-sleep",
        default = "default_error_sleep",
        with = "serde_humanize_rs"
    )]
    error_sleep: Duration,
    #[serde(rename = "max-conn", default = "default_max_conn")]
    max_conn: usize,
}

impl<Listener: Default> Default for WithListenLimits<Listener> {
    fn default() -> Self {
        Self {
            inner: Listener::default(),
            error_sleep: default_error_sleep(),
            max_conn: default_max_conn(),
        }
    }
}

impl<Listener> ListenLimits for WithListenLimits<Listener> {
    fn error_sleep(&self) -> Duration {
        self.error_sleep
    }
    fn max_conn(&self) -> usize {
        self.max_conn
    }
}

delegate_resource_traits! {
    delegate ExtraCfgCarrier, ResourceConfig to inner on WithListenLimits;
}

pub type TcpListenWithLimits<ExtraCfg = Empty, ScaleMode = Scale> =
    WithListenLimits<TcpListen<ExtraCfg, ScaleMode>>;

/// A configuration fragment describing a bound UDP socket.
///
/// This is similar to [`TcpListen`](struct.TcpListen.html), but for UDP sockets. While the action
/// on a TCP socket is called for each new accepted connection, the action for UDP socket is used
/// to handle the whole UDP socket created from this configuration.
///
/// # Type parameters
///
/// * `ExtraCfg`: Extra options folded into this configuration, for application specific options.
///   They are passed to the action.
/// * `ScaleMode`: How scaling should be done. If scaling is enabled, the action should handle
///   situation where it runs in multiple instances. However, even in case scaling is disabled, the
///   action needs to handle being „restarted“ ‒ if there's a new configuration for the socket, the
///   old future is dropped and new one, with a new socket, is created.
///
/// # Configuration options
///
/// In addition to options provided by the above type parameters, these are present:
///
/// * `host`: The hostname/interface to bind to. Defaults to `*`.
/// * `port`: The port to bind the UDP socket to (mandatory). While it is possible to create
///   unbound UDP sockets with an OS-assigned port, these don't need the configuration and are not
///   created by this configuration fragment.
///
/// #
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct UdpListen<ExtraCfg = Empty, ScaleMode: Scaled = Scale> {
    #[serde(flatten)]
    listen: Listen,
    #[serde(flatten)]
    scale: ScaleMode,
    #[serde(flatten)]
    extra_cfg: ExtraCfg,
}

impl<ExtraCfg, ScaleMode> ExtraCfgCarrier for UdpListen<ExtraCfg, ScaleMode>
where
    ScaleMode: Scaled,
{
    type Extra = ExtraCfg;
    fn extra(&self) -> &ExtraCfg {
        &self.extra_cfg
    }
}

impl<ExtraCfg, ScaleMode, O, C> ResourceConfig<O, C> for UdpListen<ExtraCfg, ScaleMode>
where
    ExtraCfg: Debug + PartialEq + Send + Sync + 'static,
    ScaleMode: Debug + PartialEq + Scaled + Send + Sync + 'static,
{
    type Seed = StdUdpSocket;
    type Resource = UdpSocket;
    fn create(&self, _: &str) -> Result<StdUdpSocket, Error> {
        self.listen.create_udp()
    }
    fn fork(&self, seed: &StdUdpSocket, _: &str) -> Result<UdpSocket, Error> {
        seed.try_clone() // Another copy of the listener
            // std → tokio socket conversion
            .and_then(|listener| UdpSocket::from_std(listener, &Handle::default()))
            .map_err(Error::from)
    }
    fn scaled(&self, name: &str) -> (usize, ValidationResults) {
        self.scale.scaled(name)
    }
    fn is_similar(&self, other: &Self, _: &str) -> bool {
        self.listen == other.listen
    }
}

cfg_helpers! {
    impl helpers for TcpListen <ExtraCfg, ScaleMode>
    where
        ExtraCfg: Debug + PartialEq + Send + Sync + 'static,
        ScaleMode: Debug + PartialEq + Scaled + Send + Sync + 'static;

    impl helpers for UdpListen <ExtraCfg, ScaleMode>
    where
        ExtraCfg: Debug + PartialEq + Send + Sync + 'static,
        ScaleMode: Debug + PartialEq + Scaled + Send + Sync + 'static;

    impl helpers for WithListenLimits<Listener> where;
}
