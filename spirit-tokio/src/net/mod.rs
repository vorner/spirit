//! Autoconfiguration of network primitives of [tokio]
//!
//! This is where the „meat“ of this crate lives. It contains various configuration fragments and
//! utilities to manage network primitives. However, currently only listening (bound) sockets.
//! Keeping a set of connecting socket (eg. a connection pool or something like that) is vaguely
//! planned, though it is not clear how it'll look exactly. Input is welcome.
//!
//! Note that many common types are reexported to the root of the crate.

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

use super::ResourceConfig;
use scaled::{Scale, Scaled};

pub mod limits;
#[cfg(unix)]
pub mod unix;

/// Abstraction over endpoints that accept connections.
///
/// The [`TcpListener`] has the [`incoming`] method that produces a stream of incoming
/// connections. This abstracts over types with similar functionality ‒ either wrapped
/// [`TcpListener`], [`UnixListener`] on unix, types that provide encryption on top of these, etc.
///
/// [`TcpListener`]: ::tokio::net::TcpListener
/// [`incoming`]: ::tokio::net::TcpListener::incoming
/// [`UnixListener`]: ::tokio::net::unix::UnixListener
pub trait IntoIncoming: Send + Sync + 'static {
    /// The type of one accepted connection.
    type Connection: Send + Sync + 'static;

    /// The stream produced.
    type Incoming: Stream<Item = Self::Connection, Error = IoError> + Send + Sync + 'static;

    /// Turns the given resource into the stream of incoming connections.
    fn into_incoming(self) -> Self::Incoming;
}

impl IntoIncoming for TcpListener {
    type Connection = TcpStream;
    type Incoming = Incoming;
    fn into_incoming(self) -> Self::Incoming {
        self.incoming()
    }
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
/// This is used as the base of the [`TcpListen`] and [`UdpListen`] configuration fragments.
///
/// # Configuration options
///
/// * `port` (mandatory)
/// * `host` (optional, if not present, `::` is used)
/// * `reuse-addr` (optional, boolean, if not present the OS default is used)
/// * `reuse-port` (optional, boolean, if not present the OS default is used, does something only
///   on unix).
/// * `only-v6` (optional, boolean, if not present the OS default is used, does nothing for IPv4
///   sockets).
/// * `backlog` (optional, number of waiting connections to be accepted in the OS queue, defaults
///   to 128)
/// * `ttl` (TTL of the listening/UDP socket).
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

/// Abstracts over a configuration subfragment that applies further settings to an already accepted
/// stream.
///
/// The fragments that accept connections can apply some configuration to them after they have been
/// accepted but before handling them to the rest of the application. The trait is generic over the
/// type of connections accepted.
///
/// The [`Empty`] fragment provides a default configuration that does nothing.
pub trait StreamConfig<S> {
    /// Applies the configuration to the stream.
    ///
    /// If the configuration fails and error is returned, the connection is dropped (and the error
    /// is logged).
    fn configure(&self, stream: &mut S) -> Result<(), IoError>;
}

impl<S> StreamConfig<S> for Empty {
    fn configure(&self, _: &mut S) -> Result<(), IoError> {
        Ok(())
    }
}

/// Configuration that can be unset, explicitly turned off or set to a duration.
///
/// Some network configuration options can be either turned off or set to a certain duration. We
/// add the ability to leave them on the OS default (which is done if the field is not present in
/// the configuration).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum MaybeDuration {
    /// Leaves the option at the OS default.
    ///
    /// Signified by not being present in the configuration.
    Unset,

    /// Turns the option off.
    ///
    /// Deserialized from `false`.
    Off,

    /// Turns the option on with given duration.
    ///
    /// Deserialized from human-readable duration specification, eg. `100ms` or `30s`.
    Duration(Duration),
}

impl Default for MaybeDuration {
    fn default() -> Self {
        MaybeDuration::Unset
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

/// An implementation of the [`StreamConfig`] trait to configure TCP connections.
///
/// This is an implementation that allows the user configure several options of accepted TCP
/// connections.
///
/// This is also the default implementation if none is specified for [`TcpListen`].
///
/// # Fields
///
/// * `tcp-nodelay` (optional, boolean, if not set uses OS default)
/// * `tcp-recv-buf-size` (optional, size of the OS buffer on the receive end of the socket,
///   if not set uses the OS default)
/// * `tcp-send-buf-size` (similar, but for the send end)
/// * `tcp-keepalive` (optional, see [`MaybeDuration`])
/// * `accepted-ttl` (optional, uses OS default if not set)
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
            MaybeDuration::Unset => (),
            MaybeDuration::Off => stream.set_keepalive(None)?,
            MaybeDuration::Duration(duration) => stream.set_keepalive(Some(duration))?,
        }
        if let Some(ttl) = self.accepted_ttl {
            stream.set_ttl(ttl)?;
        }
        Ok(())
    }
}

/// A wrapper around an [`IntoIncoming`] that applies configuration.
///
/// This is produced by [`ResourceConfig`]s that produce listeners which configure the accepted
/// connections. Usually doesn't need to be used directly (this is more of a plumbing type).
#[derive(Debug)]
pub struct ConfiguredStreamListener<Listener, Config> {
    listener: Listener,
    config: Config,
}

impl<Listener, Config> ConfiguredStreamListener<Listener, Config> {
    /// Creates a new instance from parts.
    pub fn new(listener: Listener, config: Config) -> Self {
        Self { listener, config }
    }
    /// Disassembles it into the components.
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

/// A stream wrapper that applies configuration to each item.
///
/// This is produced by the [`ConfiguredStreamListener`]. It applies the configuration to each
/// connection just before yielding it.
///
/// Can't be directly constructed and probably isn't something to interact with directly (this is
/// more of a plumbing type).
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
/// The [`ResourceConfig`] creates a [`TcpListener`] (wrapped in [`ConfiguredIncoming`]). It can be
/// handled directly, or through [`per_connection`].
///
/// Note that this stream sometimes returns errors „in the middle“, but [`per_connection`] (and
/// some consumers) terminate on the first error. You might be interested in the
/// [`WithListenLimits`] wrapper to handle that automatically (or, see the [`TcpListenWithLimits`]
/// for a convenient type alias).
///
/// It can be used directly through the [`resource`] or [`resources`] functions, or through the
/// [`CfgHelper`] and [`IteratedCfgHelper`] traits.
///
/// # Type parametes
///
/// * `ExtraCfg`: Additional application specific configuration that can be extracted through the
///   [`ExtraCfgCarrier`] trait. Defaults to an empty configuration.
/// * `ScaleMode`: The [`Scaled`] instance to specify number of instances to spawn. Defaults to
///   [`Scale`], which lets the user configure.
/// * `TcpStreamConfigure`: Further configuration for the accepted TCP streams in the form of
///   [`StreamConfig`]
///
/// # Fields
///
/// The configuration fields are pooled from all three type parameters above and from the
/// [`Listen`] configuration fragment.
///
/// [`per_connection`]: ::per_connection
/// [`resource`]: ::resource
/// [`resources`]: ::resources
/// [`ResourceConsumer`]: ::ResourceConsumer
/// [`CfgHelper`]: ::spirit::helpers::CfgHelper
/// [`IteratedCfgHelper`]: ::spirit::helpers::IteratedCfgHelper
/// [`ExtraCfgCarrier`]: ::base_traits::ExtraCfgCarrier
/// [`WithListenLimits`]: limits::WithListenLimits
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
            .map(|listener| ConfiguredStreamListener::new(listener, config))
    }
    fn scaled(&self, name: &str) -> (usize, ValidationResults) {
        self.scale.scaled(name)
    }
    fn is_similar(&self, other: &Self, _: &str) -> bool {
        self.listen == other.listen
    }
}

/// A [`TcpListen`] with all parameters set to [`Empty`].
///
/// This doesn't configure much more than the minimum actually needed.
pub type MinimalTcpListen<ExtraCfg = Empty> = TcpListen<ExtraCfg, Empty, Empty>;

/// Convenience type alias for configuration fragment for TCP listening socket with handling of
/// accept errors and limiting number of current connections.
pub type TcpListenWithLimits<ExtraCfg = Empty, ScaleMode = Scale, TcpStreamConfigure = TcpConfig> =
    limits::WithListenLimits<TcpListen<ExtraCfg, ScaleMode, TcpStreamConfigure>>;

/// A configuration fragment describing a bound UDP socket.
///
/// This is similar to [`TcpListen`](struct.TcpListen.html), but for UDP sockets.
///
/// # Type parameters
///
/// * `ExtraCfg`: Extra options folded into this configuration, for application specific options.
///   They can be extracted using the [`ExtraCfgCarrier`] trait.
/// * `ScaleMode`: An implementation of the [`Scaled`] trait, describing into how many instances
///   the socket should be scaled.
///
/// # Configuration options
///
/// In addition to options provided by the above type parameters, the options from [`Listen`] are
/// prestent.
///
/// [`ExtraCfgCarrier`]: ::base_traits::ExtraCfgCarrier
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct UdpListen<ExtraCfg = Empty, ScaleMode = Scale> {
    #[serde(flatten)]
    listen: Listen,
    #[serde(flatten)]
    scale: ScaleMode,
    #[serde(flatten)]
    extra_cfg: ExtraCfg,
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

extra_cfg_impl! {
    TcpListen<ExtraCfg, ScaleMode, TcpConfig>::extra_cfg: ExtraCfg;
    UdpListen<ExtraCfg, ScaleMode>::extra_cfg: ExtraCfg;
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
        assert_eq!(MaybeDuration::Unset, MaybeDuration::load(r#"{}"#).unwrap());
    }
}
