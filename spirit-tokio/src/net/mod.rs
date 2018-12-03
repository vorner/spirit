use std::cmp;
use std::fmt::Debug;
use std::io::Error as IoError;
use std::net::{IpAddr, TcpListener as StdTcpListener, UdpSocket as StdUdpSocket};
use std::time::Duration;

use failure::Error;
use futures::{Async, Poll, Stream};
#[cfg(unix)]
use net2::unix::{UnixTcpBuilderExt, UnixUdpBuilderExt};
use net2::{TcpBuilder, UdpBuilder};
use serde::de::{Deserialize, Deserializer, Error as DeError, Unexpected};
use serde_humanize_rs;
use spirit::validation::Results as ValidationResults;
use spirit::Empty;
use tokio::net::tcp::Incoming;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::reactor::Handle;

use super::{ExtraCfgCarrier, ResourceConfig, ResourceConsumer};
use scaled::{Scale, Scaled};

#[cfg(unix)]
pub mod unix;

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

fn default_host() -> IpAddr {
    "::".parse().unwrap()
}

fn default_backlog() -> u32 {
    128 // Number taken from rust standard library implementation
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
#[serde(rename_all = "kebab-case")]
pub struct Listen {
    port: u16,
    #[serde(default = "default_host")]
    host: IpAddr,
    reuse_addr: Option<bool>,
    reuse_port: Option<bool>,
    only_v6: Option<bool>,
    #[serde(default = "default_backlog")]
    backlog: u32,
    ttl: Option<u32>,
}

impl Default for Listen {
    fn default() -> Self {
        Listen {
            port: 0,
            host: default_host(),
            reuse_addr: None,
            reuse_port: None,
            only_v6: None,
            backlog: default_backlog(),
            ttl: None,
        }
    }
}

impl Listen {
    /// Creates a TCP socket described by the loaded configuration.
    pub fn create_tcp(&self) -> Result<StdTcpListener, Error> {
        let builder = match self.host {
            IpAddr::V4(_) => TcpBuilder::new_v4(),
            IpAddr::V6(_) => TcpBuilder::new_v6(),
        }?;
        if let Some(only_v6) = self.only_v6 {
            builder.only_v6(only_v6)?;
        }
        if let Some(reuse_addr) = self.reuse_addr {
            builder.reuse_address(reuse_addr)?;
        }
        if let Some(reuse_port) = self.reuse_port {
            if cfg!(unix) {
                builder.reuse_port(reuse_port)?;
            } else {
                warn!("reuse-port does nothing on this platform");
            }
        }
        if let Some(ttl) = self.ttl {
            builder.ttl(ttl)?;
        }
        builder.bind((self.host, self.port))?;
        Ok(builder.listen(cmp::min(self.backlog, i32::max_value() as u32) as i32)?)
    }
    /// Creates a UDP socket described by the loaded configuration.
    pub fn create_udp(&self) -> Result<StdUdpSocket, Error> {
        let builder = match self.host {
            IpAddr::V4(_) => UdpBuilder::new_v4(),
            IpAddr::V6(_) => UdpBuilder::new_v6(),
        }?;
        if let Some(only_v6) = self.only_v6 {
            builder.only_v6(only_v6)?;
        }
        if let Some(reuse_addr) = self.reuse_addr {
            builder.reuse_address(reuse_addr)?;
        }
        if let Some(reuse_port) = self.reuse_port {
            if cfg!(unix) {
                builder.reuse_port(reuse_port)?;
            } else {
                warn!("reuse-port does nothing on this platform");
            }
        }
        if let Some(ttl) = self.ttl {
            builder.ttl(ttl)?;
        }
        Ok(builder.bind((self.host, self.port))?)
    }
}

pub trait StreamConfig<S> {
    fn configure(&self, stream: &mut S) -> Result<(), IoError>;
}

impl<S> StreamConfig<S> for Empty {
    fn configure(&self, _: &mut S) -> Result<(), IoError> {
        Ok(())
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum MaybeDuration {
    Default,
    Off,
    Duration(Duration),
}

impl Default for MaybeDuration {
    fn default() -> Self {
        MaybeDuration::Default
    }
}

impl<'de> Deserialize<'de> for MaybeDuration {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Duration(#[serde(with = "serde_humanize_rs")] Duration),
            Off(bool),
        }

        let raw = Raw::deserialize(deserializer)?;
        match raw {
            Raw::Duration(duration) => Ok(MaybeDuration::Duration(duration)),
            Raw::Off(false) => Ok(MaybeDuration::Off),
            Raw::Off(true) => Err(D::Error::invalid_value(
                Unexpected::Bool(true),
                &"false or duration string",
            )),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TcpConfig {
    #[serde(rename = "tcp-nodelay")]
    nodelay: Option<bool>,
    #[serde(rename = "tcp-recv-buf-size")]
    recv_buf_size: Option<usize>,
    #[serde(rename = "tcp-send-buf-size")]
    send_buf_size: Option<usize>,
    #[serde(rename = "tcp-keepalive", default)]
    keepalive: MaybeDuration,
    #[serde(rename = "accepted-ttl")]
    accepted_ttl: Option<u32>,
}

impl StreamConfig<TcpStream> for TcpConfig {
    fn configure(&self, stream: &mut TcpStream) -> Result<(), IoError> {
        if let Some(nodelay) = self.nodelay {
            stream.set_nodelay(nodelay)?;
        }
        if let Some(recv_buf_size) = self.recv_buf_size {
            stream.set_recv_buffer_size(recv_buf_size)?;
        }
        if let Some(send_buf_size) = self.send_buf_size {
            stream.set_send_buffer_size(send_buf_size)?;
        }
        match self.keepalive {
            MaybeDuration::Default => (),
            MaybeDuration::Off => stream.set_keepalive(None)?,
            MaybeDuration::Duration(duration) => stream.set_keepalive(Some(duration))?,
        }
        if let Some(ttl) = self.accepted_ttl {
            stream.set_ttl(ttl)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct ConfiguredStreamListener<Listener, Config> {
    listener: Listener,
    config: Config,
}

impl<Listener, Config> ConfiguredStreamListener<Listener, Config> {
    pub fn into_parts(self) -> (Listener, Config) {
        (self.listener, self.config)
    }
}

impl<Listener, Config> IntoIncoming for ConfiguredStreamListener<Listener, Config>
where
    Listener: IntoIncoming + Send + Sync + 'static,
    Config: StreamConfig<Listener::Connection> + Send + Sync + 'static,
{
    type Connection = Listener::Connection;
    type Incoming = ConfiguredIncoming<Listener::Incoming, Config>;
    fn into_incoming(self) -> Self::Incoming {
        ConfiguredIncoming {
            incoming: self.listener.into_incoming(),
            config: self.config,
        }
    }
}

#[derive(Debug)]
pub struct ConfiguredIncoming<Incoming, Config> {
    incoming: Incoming,
    config: Config,
}

impl<Incoming, Config> Stream for ConfiguredIncoming<Incoming, Config>
where
    Incoming: Stream<Error = IoError> + Send + Sync + 'static,
    Config: StreamConfig<Incoming::Item>,
{
    type Item = Incoming::Item;
    type Error = IoError;
    fn poll(&mut self) -> Poll<Option<Incoming::Item>, IoError> {
        match self.incoming.poll() {
            Ok(Async::Ready(Some(mut stream))) => {
                self.config.configure(&mut stream)?;
                Ok(Async::Ready(Some(stream)))
            }
            other => other,
        }
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
pub struct TcpListen<ExtraCfg = Empty, ScaleMode = Scale, TcpStreamConfigure = TcpConfig> {
    #[serde(flatten)]
    listen: Listen,
    #[serde(flatten)]
    tcp_config: TcpStreamConfigure,
    #[serde(flatten)]
    scale: ScaleMode,
    #[serde(flatten)]
    extra_cfg: ExtraCfg,
}

impl<ExtraCfg, ScaleMode, TcpConfig> ExtraCfgCarrier for TcpListen<ExtraCfg, ScaleMode, TcpConfig> {
    type Extra = ExtraCfg;
    fn extra(&self) -> &ExtraCfg {
        &self.extra_cfg
    }
}

impl<ExtraCfg, ScaleMode, TcpConfig, O, C> ResourceConfig<O, C>
    for TcpListen<ExtraCfg, ScaleMode, TcpConfig>
where
    ExtraCfg: Debug + PartialEq + Send + Sync + 'static,
    ScaleMode: Debug + PartialEq + Scaled + Send + Sync + 'static,
    TcpConfig: Clone + Debug + PartialEq + Send + Sync + StreamConfig<TcpStream> + 'static,
{
    type Seed = StdTcpListener;
    type Resource = ConfiguredStreamListener<TcpListener, TcpConfig>;
    fn create(&self, _: &str) -> Result<StdTcpListener, Error> {
        self.listen.create_tcp()
    }
    fn fork(&self, seed: &StdTcpListener, _: &str) -> Result<Self::Resource, Error> {
        let config = self.tcp_config.clone();
        seed.try_clone() // Another copy of the listener
            // std → tokio socket conversion
            .and_then(|listener| TcpListener::from_std(listener, &Handle::default()))
            .map_err(Error::from)
            .map(|listener| ConfiguredStreamListener { listener, config })
    }
    fn scaled(&self, name: &str) -> (usize, ValidationResults) {
        self.scale.scaled(name)
    }
    fn is_similar(&self, other: &Self, _: &str) -> bool {
        self.listen == other.listen
    }
}

pub type MinimalTcpListen<ExtraCfg = Empty> = TcpListen<ExtraCfg, Empty, Empty>;

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

pub type TcpListenWithLimits<ExtraCfg = Empty, ScaleMode = Scale, TcpStreamConfigure = TcpConfig> =
    WithListenLimits<TcpListen<ExtraCfg, ScaleMode, TcpStreamConfigure>>;

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
        seed.try_clone() // Another copy of the socket
            // std → tokio socket conversion
            .and_then(|socket| UdpSocket::from_std(socket, &Handle::default()))
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
    impl helpers for TcpListen <ExtraCfg, ScaleMode, TcpStreamConfigure>
    where
        ExtraCfg: Debug + PartialEq + Send + Sync + 'static,
        ScaleMode: Debug + PartialEq + Scaled + Send + Sync + 'static,
        TcpConfig: Debug + PartialEq + Send + Sync + 'static;

    impl helpers for UdpListen <ExtraCfg, ScaleMode>
    where
        ExtraCfg: Debug + PartialEq + Send + Sync + 'static,
        ScaleMode: Debug + PartialEq + Scaled + Send + Sync + 'static;

    impl helpers for WithListenLimits<Listener> where;
}

#[cfg(test)]
mod tests {
    extern crate serde_json;

    use self::serde_json::error::Error as JsonError;

    use super::*;

    impl MaybeDuration {
        fn load(json: &str) -> Result<Self, JsonError> {
            #[derive(Debug, Deserialize)]
            struct Wrap {
                #[serde(default)]
                maybe: MaybeDuration,
            }

            serde_json::from_str::<Wrap>(json).map(|Wrap { maybe }| maybe)
        }
    }

    #[test]
    fn maybe_duration_time() {
        assert_eq!(
            MaybeDuration::Duration(Duration::from_millis(12)),
            MaybeDuration::load(r#"{"maybe": "12ms"}"#).unwrap(),
        );
    }

    #[test]
    fn maybe_duration_off() {
        assert_eq!(
            MaybeDuration::Off,
            MaybeDuration::load(r#"{"maybe": false}"#).unwrap()
        );
    }

    #[test]
    fn maybe_duration_default() {
        assert_eq!(
            MaybeDuration::Default,
            MaybeDuration::load(r#"{}"#).unwrap()
        );
    }
}
