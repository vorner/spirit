//! Autoconfiguration of network primitives of [tokio]
//!
//! Available only with the `net` feature. This contains several [`Fragment`]s for configuring
//! network primitives.
//!
//! [`Fragment`]: spirit::Fragment

use std::cmp;
use std::fmt::Debug;
use std::future::Future;
use std::io::Error as IoError;
use std::net::{IpAddr, SocketAddr, TcpListener as StdTcpListener, UdpSocket as StdUdpSocket};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use err_context::prelude::*;
use err_context::AnyError;
#[cfg(not(unix))]
use log::warn;
#[cfg(unix)]
use serde::de::{Deserializer, Error as DeError, Unexpected};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use socket2::{Domain, Socket, Type as SocketType};
use spirit::fragment::driver::{CacheSimilar, Comparable, Comparison};
use spirit::fragment::{Fragment, Stackable};
use spirit::Empty;
#[cfg(feature = "cfg-help")]
use structdoc::StructDoc;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
#[cfg(feature = "stream")]
use tokio::stream::Stream;

pub mod limits;
#[cfg(unix)]
pub mod unix;

// TODO: Unix domain sockets
// TODO: Either?

/// Configuration that can be unset, explicitly turned off or set to a duration.
///
/// Some network configuration options can be either turned off or set to a certain duration. We
/// add the ability to leave them on the OS default (which is done if the field is not present in
/// the configuration).
///
/// # Panics
///
/// If using on your own and using `Serialize`, make sure that you use
/// `#[serde(skip_serializing_if = "MaybeDuration::is_unset")]`, or you'll get a panic during
/// serialization.
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

impl MaybeDuration {
    fn is_unset(&self) -> bool {
        *self == MaybeDuration::Unset
    }
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
            Duration(#[serde(deserialize_with = "spirit::utils::deserialize_duration")] Duration),
            Off(Option<bool>),
        }

        let raw = Raw::deserialize(deserializer)?;
        match raw {
            Raw::Duration(duration) => Ok(MaybeDuration::Duration(duration)),
            Raw::Off(Some(false)) => Ok(MaybeDuration::Off),
            Raw::Off(Some(true)) => Err(D::Error::invalid_value(
                Unexpected::Bool(true),
                &"false or duration string",
            )),
            Raw::Off(None) => Ok(MaybeDuration::Unset),
        }
    }
}

impl Serialize for MaybeDuration {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            MaybeDuration::Unset => s.serialize_none(),
            MaybeDuration::Off => s.serialize_bool(false),
            MaybeDuration::Duration(d) => {
                s.serialize_str(&humantime::format_duration(*d).to_string())
            }
        }
    }
}

/// A plumbing type, return value of [`Accept::accept`].
pub struct AcceptFuture<'a, A: ?Sized>(&'a mut A);

impl<A: Accept> Future for AcceptFuture<'_, A> {
    type Output = Result<A::Connection, IoError>;

    fn poll(mut self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Self::Output> {
        self.0.poll_accept(ctx)
    }
}

/// Abstraction over endpoints that accept connections.
pub trait Accept: Send + Sync + 'static {
    /// The type of the accepted connection.
    type Connection: Send + Sync + 'static;

    /// Poll for availability of the next connection.
    ///
    /// In case there's none ready, it returns [`Poll::Pending`] and the current task will be
    /// notified by a waker.
    fn poll_accept(&mut self, ctx: &mut Context) -> Poll<Result<Self::Connection, IoError>>;

    /// Accept the next connection.
    ///
    /// Returns a future that will yield the next connection.
    fn accept(&mut self) -> AcceptFuture<Self> {
        AcceptFuture(self)
    }
}

impl Accept for TcpListener {
    type Connection = TcpStream;

    fn poll_accept(&mut self, ctx: &mut Context) -> Poll<Result<TcpStream, IoError>> {
        match self.poll_accept(ctx) {
            Poll::Ready(Ok((stream, _addr))) => Poll::Ready(Ok(stream)),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
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
/// but isn't very useful. It'll listen on a OS-assigned port on all the interfaces.
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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub struct Listen {
    /// The port to bind to.
    pub port: u16,

    /// The interface to bind to.
    ///
    /// Defaults to `::` (IPv6 any).
    #[serde(default = "default_host")]
    pub host: IpAddr,

    /// The SO_REUSEADDR socket option.
    ///
    /// Usually, the OS reserves the host-port pair for a short time after it has been released, so
    /// leftovers of old connections don't confuse the new application. The SO_REUSEADDR option
    /// asks the OS not to do this reservation.
    ///
    /// If not set, it is left on the OS default (which is likely off = doing the reservation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reuse_addr: Option<bool>,

    /// The SO_REUSEPORT socket option.
    ///
    /// Setting this to true allows multiple applications to *simultaneously* bind to the same
    /// port.
    ///
    /// If not set, it is left on the OS default (which is likely off).
    ///
    /// This option is not available on Windows and has no effect there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reuse_port: Option<bool>,

    /// The IP_V6ONLY option.
    ///
    /// This has no effect on IPv4 sockets.
    ///
    /// On IPv6 sockets, this restricts the socket to sending and receiving only IPv6 packets. This
    /// allows another socket to bind the same port on IPv4. If it is set to false, the socket can
    /// communicate with both IPv6 and IPv4 endpoints under some circumstances.
    ///
    /// Due to platform differences, the generally accepted best practice is to bind IPv4 and IPv6
    /// as two separate sockets, the IPv6 one with setting IP_V6ONLY explicitly to true.
    ///
    /// If not set, it is left on the OS default (which differs from OS to OS).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only_v6: Option<bool>,

    /// The accepting backlog.
    ///
    /// Has no effect for UDP sockets.
    ///
    /// This specifies how many connections can wait in the kernel before being accepted. If more
    /// than this limit are queued, the kernel starts refusing them with connection reset packets.
    ///
    /// The default is 128.
    #[serde(default = "default_backlog")]
    pub backlog: u32,

    /// The TTL field of IP packets sent from this socket.
    ///
    /// If not set, it defaults to the OS value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<u32>,
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
    fn create_any(&self, tp: SocketType) -> Result<Socket, AnyError> {
        let domain = match self.host {
            IpAddr::V4(_) => Domain::ipv4(),
            IpAddr::V6(_) => Domain::ipv6(),
        };
        let socket = Socket::new(domain, tp, None).context("Creating socket")?;
        if let Some(only_v6) = self.only_v6 {
            socket
                .set_only_v6(only_v6)
                .context("Setting IPV6_V6ONLY flag")?;
        }
        if let Some(reuse_addr) = self.reuse_addr {
            socket
                .set_reuse_address(reuse_addr)
                .context("Setting SO_REUSEADDR flag")?;
        }
        if let Some(reuse_port) = self.reuse_port {
            #[cfg(unix)]
            socket
                .set_reuse_port(reuse_port)
                .context("Setting SO_REUSEPORT flag")?;
            #[cfg(not(unix))]
            warn!("reuse-port does nothing on this platform");
        }
        if let Some(ttl) = self.ttl {
            socket.set_ttl(ttl)?;
        }
        let addr = SocketAddr::from((self.host, self.port));
        socket
            .bind(&addr.into())
            .with_context(|_| format!("Binding socket to {}", addr))?;
        Ok(socket)
    }
    /// Creates a TCP socket described by the loaded configuration.
    ///
    /// This is the synchronous socket from standard library. See [`TcpListener::from_std`].
    pub fn create_tcp(&self) -> Result<StdTcpListener, AnyError> {
        let sock = self.create_any(SocketType::stream())?;
        sock.listen(cmp::min(self.backlog, i32::max_value() as u32) as i32)
            .context("Listening to TCP socket")?;
        Ok(sock.into_tcp_listener())
    }

    /// Creates a UDP socket described by the loaded configuration.
    ///
    /// This is the synchronous socket from standard library. See [`UdpSocket::from_std`].
    pub fn create_udp(&self) -> Result<StdUdpSocket, AnyError> {
        let sock = self.create_any(SocketType::dgram())?;
        Ok(sock.into_udp_socket())
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
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[non_exhaustive]
pub struct TcpConfig {
    /// The TCP_NODELAY option.
    ///
    /// If set to true, packets are sent as soon as there are some data queued by the application.
    /// This is faster, but may create more (undersized) packets in some circumstances.
    ///
    /// Setting this to false sends the first undersized packet, but then it waits to either
    /// receive ACK from the other side or for enough data to fill a whole packet before sending
    /// another.
    ///
    /// Left to OS default if not set.
    #[serde(rename = "tcp-nodelay", skip_serializing_if = "Option::is_none")]
    pub nodelay: Option<bool>,

    /// The receive buffer size of the connection, in bytes.
    ///
    /// Left to OS default if not set.
    #[serde(rename = "tcp-recv-buf-size", skip_serializing_if = "Option::is_none")]
    pub recv_buf_size: Option<usize>,

    /// The send buffer size of the connection, in bytes.
    ///
    /// Left to the OS default if not set.
    #[serde(rename = "tcp-send-buf-size", skip_serializing_if = "Option::is_none")]
    pub send_buf_size: Option<usize>,

    /// The TCP keepalive time.
    ///
    /// If set to interval (for example `5m` or `30s`), a TCP keepalive packet will be sent every
    /// this often. If set to `false`, TCP keepalive will be turned off.
    ///
    /// Left to the OS default if not set.
    #[serde(
        rename = "tcp-keepalive",
        default,
        skip_serializing_if = "MaybeDuration::is_unset"
    )]
    #[cfg_attr(feature = "cfg-help", structdoc(leaf = "Time interval/false"))]
    pub keepalive: MaybeDuration,

    /// The IP TTL field of packets sent through an accepted connection.
    ///
    /// Left to the OS default if not set.
    #[serde(rename = "accepted-ttl", skip_serializing_if = "Option::is_none")]
    pub accepted_ttl: Option<u32>,
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

/// A wrapper that applies configuration to each accepted connection.
#[derive(Debug)]
pub struct ConfiguredListener<A, C> {
    accept: A,
    config: C,
}

impl<A, C> ConfiguredListener<A, C> {
    /// Creates a new instance from parts.
    pub fn new(accept: A, config: C) -> Self {
        Self { accept, config }
    }

    /// Disassembles it into the components.
    pub fn into_parts(self) -> (A, C) {
        (self.accept, self.config)
    }
}

impl<A, C> Accept for ConfiguredListener<A, C>
where
    A: Accept,
    C: StreamConfig<A::Connection> + Send + Sync + 'static,
{
    type Connection = A::Connection;
    fn poll_accept(&mut self, ctx: &mut Context) -> Poll<Result<A::Connection, IoError>> {
        match self.accept.poll_accept(ctx) {
            Poll::Ready(Ok(mut conn)) => {
                if let Err(e) = self.config.configure(&mut conn) {
                    return Poll::Ready(Err(e));
                }
                Poll::Ready(Ok(conn))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(feature = "stream")]
impl<A, C> Stream for ConfiguredListener<A, C>
where
    A: Accept + Unpin,
    C: StreamConfig<A::Connection> + Unpin + Send + Sync + 'static,
{
    type Item = Result<A::Connection, IoError>;
    fn poll_next(mut self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Option<Self::Item>> {
        self.poll_accept(ctx).map(Some)
    }
}

/// A configuration fragment of a TCP listening socket.
///
/// The [`Fragment`] creates a [`TcpListener`] (wrapped in [`ConfiguredListener`]). It can be
/// handled directly, or through [`Pipeline`]s.
///
/// Note that this stream sometimes returns errors „in the middle“, but most stream consumers
/// terminate on the first error. You might be interested in the [`WithListenLimits`] wrapper to
/// handle that automatically (or, see the [`TcpListenWithLimits`]
/// for a convenient type alias).
///
/// # Type parametes
///
/// * `ExtraCfg`: Additional application specific configuration that can be extracted and used by
///   the application code. This doesn't influence the socket created in any way.
/// * `TcpStreamConfigure`: Further configuration for the accepted TCP streams in the form of
///   [`StreamConfig`]
///
/// # Fields
///
/// The configuration fields are pooled from all three type parameters above and from the
/// [`Listen`] configuration fragment.
///
/// [`Pipeline`]: spirit::fragment::Pipeline.
/// [`handlers`]: crate::handlers
/// [`WithListenLimits`]: crate::net::limits::WithListenLimits
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[non_exhaustive]
pub struct TcpListen<ExtraCfg = Empty, TcpStreamConfigure = TcpConfig> {
    /// The actual listener socket address.
    #[serde(flatten)]
    pub listen: Listen,

    /// Configuration to be applied to the accepted connections.
    #[serde(flatten)]
    pub tcp_config: TcpStreamConfigure,

    /// Arbitrary application specific configuration that doesn't influence the sockets created.
    #[serde(flatten)]
    pub extra_cfg: ExtraCfg,
}

impl<ExtraCfg, TcpConfig> Stackable for TcpListen<ExtraCfg, TcpConfig> {}

impl<ExtraCfg: PartialEq, TcpConfig: PartialEq> Comparable for TcpListen<ExtraCfg, TcpConfig> {
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

impl<ExtraCfg, TcpConfig> Fragment for TcpListen<ExtraCfg, TcpConfig>
where
    ExtraCfg: Clone + Debug + PartialEq,
    TcpConfig: Clone + Debug + PartialEq,
{
    type Driver = CacheSimilar<Self>;
    type Installer = ();
    type Seed = StdTcpListener;
    type Resource = ConfiguredListener<TcpListener, TcpConfig>;
    fn make_seed(&self, name: &str) -> Result<StdTcpListener, AnyError> {
        self.listen
            .create_tcp()
            .with_context(|_| format!("Failed to create STD socket {}/{:?}", name, self))
            .map_err(AnyError::from)
    }
    fn make_resource(&self, seed: &mut Self::Seed, name: &str) -> Result<Self::Resource, AnyError> {
        let config = self.tcp_config.clone();
        seed.try_clone() // Another copy of the listener
            // std → tokio socket conversion
            .and_then(TcpListener::from_std)
            .with_context(|_| format!("Failed to make socket {}/{:?} asynchronous", name, self))
            .map_err(AnyError::from)
            .map(|listener| ConfiguredListener::new(listener, config))
    }
}

/// A [`TcpListen`] with all parameters set to [`Empty`].
///
/// This doesn't configure much more than the minimum actually needed.
pub type MinimalTcpListen<ExtraCfg = Empty> = TcpListen<ExtraCfg, Empty>;

/// Convenience type alias for configuration fragment for TCP listening socket with handling of
/// accept errors and limiting number of current connections.
///
/// You might prefer this one over plain [`TcpListen`].
pub type TcpListenWithLimits<ExtraCfg = Empty, TcpStreamConfigure = TcpConfig> =
    limits::WithLimits<TcpListen<ExtraCfg, TcpStreamConfigure>>;

/// A configuration fragment describing a bound UDP socket.
///
/// This is similar to [`TcpListen`](struct.TcpListen.html), but for UDP sockets.
///
/// # Type parameters
///
/// * `ExtraCfg`: Extra options folded into this configuration, for application specific options.
///   They don't influence the socket in any way.
///
/// # Configuration options
///
/// In addition to options provided by the above type parameters, the options from [`Listen`] are
/// prestent.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[non_exhaustive]
pub struct UdpListen<ExtraCfg = Empty> {
    /// Configuration for the address to bind to.
    #[serde(flatten)]
    pub listen: Listen,

    /// Arbitrary application specific configuration that doesn't influence the created socket, but
    /// can be examined in the [`handlers`].
    ///
    /// [`handlers`]: crate::handlers
    #[serde(flatten)]
    pub extra_cfg: ExtraCfg,
}

impl<ExtraCfg> Stackable for UdpListen<ExtraCfg> {}

impl<ExtraCfg: PartialEq> Comparable for UdpListen<ExtraCfg> {
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

impl<ExtraCfg> Fragment for UdpListen<ExtraCfg>
where
    ExtraCfg: Clone + Debug + PartialEq,
{
    type Driver = CacheSimilar<Self>;
    type Installer = ();
    type Seed = StdUdpSocket;
    type Resource = UdpSocket;
    fn make_seed(&self, name: &str) -> Result<Self::Seed, AnyError> {
        self.listen
            .create_udp()
            .with_context(|_| format!("Failed to create STD socket {}/{:?}", name, self))
            .map_err(AnyError::from)
    }
    fn make_resource(&self, seed: &mut Self::Seed, name: &str) -> Result<UdpSocket, AnyError> {
        seed.try_clone() // Another copy of the socket
            // std → tokio socket conversion
            .and_then(UdpSocket::from_std)
            .with_context(|_| format!("Failed to make socket {}/{:?} async", name, self))
            .map_err(AnyError::from)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::error::Error as JsonError;

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

    #[test]
    fn maybe_duration_nil() {
        assert_eq!(
            MaybeDuration::Unset,
            MaybeDuration::load(r#"{"maybe": null}"#).unwrap()
        );
    }
}
