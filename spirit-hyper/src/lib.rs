#![doc(
    html_root_url = "https://docs.rs/spirit-hyper/0.7.1/spirit_hyper/",
    test(attr(deny(warnings)))
)]
// Our program-long snippets are more readable with main
#![allow(clippy::needless_doctest_main)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

//! [Spirit][spirit] extension for Hyper servers.
//!
//! This allows having Hyper servers auto-spawned from configuration. It is possible to put them on
//! top of arbitrary stream-style IO objects (TcpStream, UdsStream, these wrapped in SSL...).
//!
//! # Tokio runtime
//!
//! This uses the [`spirit_tokio`] crate under the hood. Similar drawback with initializing a
//! runtime applies here too (see the [`spirit_tokio`] docs for details).
//!
//! # Examples
//!
//! ```rust
//! use hyper::{Body, Request, Response};
//! use serde::Deserialize;
//! use spirit::{Empty, Pipeline, Spirit};
//! use spirit::prelude::*;
//! use spirit_hyper::{server_from_handler, BuildServer, HttpServer};
//!
//! const DEFAULT_CONFIG: &str = r#"
//! [server]
//! port = 2234
//! "#;
//!
//! #[derive(Default, Deserialize)]
//! struct Config {
//!     server: HttpServer,
//! }
//!
//! impl Config {
//!     fn server(&self) -> HttpServer {
//!         self.server.clone()
//!     }
//! }
//!
//! async fn request(_req: Request<Body>) -> Response<Body> {
//!     Response::new(Body::from("Hello world\n"))
//! }
//!
//! fn main() {
//!     Spirit::<Empty, Config>::new()
//!         .config_defaults(DEFAULT_CONFIG)
//!         .with(
//!             // Let's build a http server as configured by the user
//!             Pipeline::new("listen")
//!                 .extract_cfg(Config::server)
//!                 // This is where we teach the server what it serves. It is the usual stuff from
//!                 // hyper.
//!                 .transform(BuildServer(server_from_handler(request)))
//!         )
//!         .run(|spirit| {
//! #           let spirit = std::sync::Arc::clone(spirit);
//! #           std::thread::spawn(move || spirit.terminate());
//!             Ok(())
//!         });
//! }
//! ```
//!
//! Further examples (with more flexible handling) are in the
//! [git repository](https://github.com/vorner/spirit/tree/master/spirit-hyper/examples).

use std::convert::Infallible;
use std::fmt::Debug;
use std::future::Future;
use std::io::Error as IoError;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use err_context::prelude::*;
use err_context::AnyError;
use hyper::body::Body;
use hyper::server::accept::Accept as HyperAccept;
use hyper::server::{Builder, Server};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Error as HyperError, Request, Response};
use log::{debug, trace};
use serde::{Deserialize, Serialize};
use spirit::fragment::driver::{CacheSimilar, Comparable, Comparison};
use spirit::fragment::{Fragment, Stackable, Transformation};
use spirit::utils::{deserialize_opt_duration, serialize_opt_duration};
use spirit::{log_error, Empty};
use spirit_tokio::net::limits::WithLimits;
use spirit_tokio::net::{Accept as SpiritAccept, TcpListen};
use spirit_tokio::FutureInstaller;
#[cfg(feature = "cfg-help")]
use structdoc::StructDoc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::oneshot::{self, Receiver, Sender};
use tokio::task::JoinHandle;

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    v == &T::default()
}

/// Configuration of the selected HTTP protocol version.
#[derive(Copy, Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum HttpMode {
    /// Enable both HTTP1 and HTTP2 protocols.
    Both,

    /// Disable the HTTP2 protocol.
    #[serde(rename = "http1-only")]
    Http1Only,

    /// Disable the HTTP1 protocol.
    #[serde(rename = "http2-only")]
    Http2Only,
}

impl Default for HttpMode {
    fn default() -> Self {
        HttpMode::Both
    }
}

/// Configuration of Hyper HTTP servers.
///
/// This are the things that are extra over the transport. It doesn't contain any kind of ports or
/// SSL certificates, these are added inside the [`HyperServer`]. This is only for configuring the
/// HTTP protocol itself.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[serde(rename_all = "kebab-case", default)]
#[non_exhaustive]
pub struct HyperCfg {
    /// The HTTP keepalive.
    ///
    /// https://en.wikipedia.org/wiki/HTTP_persistent_connection.
    ///
    /// Default is on, can be turned off.
    pub http1_keepalive: bool,

    /// Vectored writes of headers.
    ///
    /// This is a low-level optimization setting. Using the vectored writes saves some copying of
    /// data around, but can be slower on some systems or transports.
    ///
    /// Default is on, can be turned off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http1_writev: Option<bool>,

    /// When a http1 client closes its write end, keep the connection open until the reply is sent.
    ///
    /// If set to false, if the client closes its connection, server does too.
    pub http1_half_close: bool,

    /// Maximum buffer size of HTTP1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http1_max_buf_size: Option<usize>,

    /// Initial window size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http2_initial_stream_window_size: Option<u32>,

    /// Initial window size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http2_initial_connection_window_size: Option<u32>,

    /// Choose the window sizes dynamically at runtime.
    ///
    /// If turned off (the default), uses the values configured.
    #[serde(default, skip_serializing_if = "is_default")]
    pub http2_adaptive_window: bool,

    /// Maximum number of concurrent streams.
    ///
    /// Defaults to no limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http2_max_concurrent_streams: Option<u32>,

    /// How often to send keep alive/ping frames.
    ///
    /// Defaults to disabled.
    #[serde(
        deserialize_with = "deserialize_opt_duration",
        serialize_with = "serialize_opt_duration",
        skip_serializing_if = "Option::is_none"
    )]
    pub http2_keep_alive_interval: Option<Duration>,

    /// Close connection if no response for ping in this time.
    ///
    /// Defaults to 20s. Null disables.
    pub http2_keep_alive_timeout: Duration,

    /// What protocols are enabled.
    #[serde(default)]
    pub http_mode: HttpMode,
}

impl HyperCfg {
    /// Constructs a Hyper server [`Builder`] based on this configuration.
    pub fn builder<I>(&self, incoming: I) -> Builder<I> {
        let (h1_only, h2_only) = match self.http_mode {
            HttpMode::Both => (false, false),
            HttpMode::Http1Only => (true, false),
            HttpMode::Http2Only => (false, true),
        };

        let mut builder = Server::builder(incoming)
            .http1_keepalive(self.http1_keepalive)
            .http1_half_close(self.http1_half_close)
            .http2_initial_connection_window_size(self.http2_initial_connection_window_size)
            .http2_initial_stream_window_size(self.http2_initial_stream_window_size)
            .http2_adaptive_window(self.http2_adaptive_window)
            .http2_keep_alive_interval(self.http2_keep_alive_interval)
            .http2_keep_alive_timeout(self.http2_keep_alive_timeout)
            .http1_only(h1_only)
            .http2_only(h2_only);

        if let Some(size) = self.http1_max_buf_size {
            builder = builder.http1_max_buf_size(size);
        }

        if let Some(writev) = self.http1_writev {
            builder = builder.http1_writev(writev);
        }

        builder
    }
}

impl Default for HyperCfg {
    fn default() -> Self {
        HyperCfg {
            http1_keepalive: true,
            http1_writev: None,
            http1_half_close: true,
            http1_max_buf_size: None,
            http2_initial_connection_window_size: None,
            http2_initial_stream_window_size: None,
            http2_adaptive_window: false,
            http2_max_concurrent_streams: None,
            http2_keep_alive_interval: None,
            http2_keep_alive_timeout: Duration::from_secs(20),
            http_mode: HttpMode::default(),
        }
    }
}

/// A plumbing wrapper type.
///
/// Not of direct interest to users, though it might leak to some function signatures at an
/// occasion. The real listener socket is wrapped inside.
#[derive(Copy, Clone, Debug)]
pub struct Acceptor<A>(A);

impl<A: SpiritAccept + Unpin> HyperAccept for Acceptor<A> {
    type Conn = A::Connection;
    type Error = IoError;
    fn poll_accept(
        mut self: Pin<&mut Self>,
        ctx: &mut Context,
    ) -> Poll<Option<Result<Self::Conn, IoError>>> {
        self.0.poll_accept(ctx).map(|p| p.map(Some).transpose())
    }
}

/// A [`Fragment`] for hyper servers.
///
/// This is a wrapper around a `Transport` [`Fragment`]. It takes something that accepts
/// connections ‒ like [`TcpListen`] and adds configuration specific for a HTTP server.
///
/// The [`Fragment`] produces [hyper] [Builder]. The [`BuildServer`] transformation can be used to
/// make it into a [`Server`] and install it into a tokio runtime.
///
/// See also the [`HttpServer`] type alias.
///
/// # Configuration options
///
/// In addition to options already provided by the `Transport`, these options are added:
///
/// * `http1-keepalive`: boolean, default true.
/// * `http1-writev`: boolean, default true.
/// * `http-mode`: One of `"both"`, `"http1-only"` or `"http2-only"`. Defaults to `"both"`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub struct HyperServer<Transport> {
    /// The inner transport.
    ///
    /// This is accessible by the user in case it contains something of use to the
    /// [`Transformation`]s.
    #[serde(flatten)]
    pub transport: Transport,

    /// Configuration of Hyper.
    ///
    /// The HTTP configuration is inside this.
    #[serde(flatten)]
    pub hyper_cfg: HyperCfg,
}

impl<Transport: Comparable> Comparable for HyperServer<Transport> {
    fn compare(&self, other: &Self) -> Comparison {
        let transport_cmp = self.transport.compare(&other.transport);
        if transport_cmp == Comparison::Same && self.hyper_cfg != other.hyper_cfg {
            Comparison::Similar
        } else {
            transport_cmp
        }
    }
}

impl<Transport> Fragment for HyperServer<Transport>
where
    Transport: Fragment + Debug + Clone + Comparable,
{
    type Driver = CacheSimilar<Self>;
    type Installer = ();
    type Seed = Transport::Seed;
    type Resource = Builder<Acceptor<Transport::Resource>>;
    fn make_seed(&self, name: &'static str) -> Result<Self::Seed, AnyError> {
        self.transport.make_seed(name)
    }
    fn make_resource(
        &self,
        seed: &mut Self::Seed,
        name: &'static str,
    ) -> Result<Self::Resource, AnyError> {
        debug!("Creating HTTP server {}", name);

        let transport = self.transport.make_resource(seed, name)?;
        let builder = self.hyper_cfg.builder(Acceptor(transport));

        Ok(builder)
    }
}

impl<Transport> Stackable for HyperServer<Transport> where Transport: Stackable {}

/// A type alias for http (plain TCP) hyper server.
pub type HttpServer<ExtraCfg = Empty> = HyperServer<WithLimits<TcpListen<ExtraCfg>>>;

/// A plumbing helper type.
pub struct Activate<Fut> {
    build_server: Option<Box<dyn FnOnce(Receiver<()>) -> Fut + Send>>,
    sender: Option<Sender<()>>,
    join: Option<JoinHandle<()>>,
    name: &'static str,
}

impl<Fut> Drop for Activate<Fut> {
    fn drop(&mut self) {
        // Tell the server to terminate
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(());
        }
    }
}

impl<Fut, E> Future for Activate<Fut>
where
    Fut: Future<Output = Result<(), E>> + Send + 'static,
    E: Into<AnyError>,
{
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, ctx: &mut Context) -> Poll<()> {
        if let Some(build_server) = self.build_server.take() {
            // Hack: Hyper is very unfriendly to constraining type parameters (not only there's
            // like a whole paragraph on their methods, some of the traits are private so one can't
            // really ask for them :-(.
            //
            // We don't want to box our future, and we want to remotely ask for graceful shutdown.
            // Therefore we spawn the returned future separately and signal the shutdown from our
            // drop.
            //
            // We also propagate termination of that server to us.
            let (sender, receiver) = oneshot::channel();
            let server = build_server(receiver);
            let name = self.name;
            let server = async move {
                if let Err(e) = server.await {
                    log_error!(
                        Error,
                        e.into()
                            .context(format!("HTTP server error {}", name))
                            .into()
                    );
                }
            };
            let join = tokio::spawn(server);
            self.join = Some(join);
            self.sender = Some(sender);
        }

        match Pin::new(self.join.as_mut().expect("Missing join handle")).poll(ctx) {
            Poll::Ready(Ok(())) => {
                debug!("Future of server {} terminated", self.name);
                Poll::Ready(())
            }
            Poll::Ready(Err(e)) => {
                log_error!(
                    Error,
                    e.context(format!("HTTP server {} failed", self.name))
                        .into()
                );
                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// A [`Transformation`] to turn a [`Builder`] into a [`Server`].
///
/// The value wrapped in this shall be a closure taking:
/// * The [`Builder`]
/// * The configuration fragment ([`HyperServer`])
/// * A `&str` name
/// * A [`Receiver<()>`][Receiver].
///
/// It shall produce a server wrapped with a graceful shutdown. Technically, it can produce
/// whatever future that'll terminate once the [`Receiver`] contains something.
///
/// Constructing the server builder directly is a bit of a chore (bunch of cloning, lifetimes, and
/// it's something like 3rd-order function ‒ function that returns a function that returns a
/// function...). In many cases, the [`server_from_handler`] can help.
///
/// An instance of [`ServerBuilder`] goes inside.
pub struct BuildServer<BS>(pub BS);

impl<Tr, Inst, BS> Transformation<Builder<Acceptor<Tr::Resource>>, Inst, HyperServer<Tr>>
    for BuildServer<BS>
where
    Tr: Fragment + Clone + Send + 'static,
    Tr::Resource: Send,
    BS: ServerBuilder<Tr> + Clone + Send + 'static,
    BS::OutputFut: Future<Output = Result<(), HyperError>>,
{
    type OutputResource = Activate<BS::OutputFut>;
    type OutputInstaller = FutureInstaller;
    fn installer(&mut self, _ii: Inst, _name: &'static str) -> Self::OutputInstaller {
        FutureInstaller::default()
    }
    fn transform(
        &mut self,
        builder: Builder<Acceptor<Tr::Resource>>,
        cfg: &HyperServer<Tr>,
        name: &'static str,
    ) -> Result<Self::OutputResource, AnyError> {
        let build_server = self.0.clone();
        let cfg = cfg.clone();
        let build_server = move |receiver| build_server.build(builder, &cfg, name, receiver);
        Ok(Activate {
            build_server: Some(Box::new(build_server)),
            join: None,
            name,
            sender: None,
        })
    }
}

/// A trait abstracting the creation of servers.
///
/// When spawning a server, there are 3 layers.
///
/// * A layer creating the [`Server`] from the builder.
/// * A layer creating a service for each connection.
/// * A layer responding to one request.
///
/// Each layer must be able to create new instances of the lower layer (by cloning, creating new
/// instances, etc).
///
/// This represents the top-level layer. This shall do:
///
/// * Create the [`Server`].
/// * Call the [`with_graceful_shutdown`][Server::with_graceful_shutdown] on it, tying it to the
///   passed `shutdown` parameter.
///
/// You don't have to implement the trait by hand, a closure with the corresponding signature (see
/// [`build`][ServerBuilder::build]) does the job.
///
/// This exists for two reasons:
///
/// * To enable different implementations than just closures.
/// * To allow it to live in `impl Trait` position.
///
/// # Examples
///
/// ```
/// use std::convert::Infallible;
///
/// use hyper::{Body, Request, Response};
/// use hyper::server::Builder;
/// use hyper::service::{make_service_fn, service_fn};
/// use serde::Deserialize;
/// use spirit::{Empty, Pipeline, Spirit};
/// use spirit::prelude::*;
/// use spirit_hyper::{BuildServer, HttpServer};
/// use spirit_tokio::net::limits::Tracked;
/// use tokio::net::TcpStream;
/// use tokio::sync::oneshot::Receiver;
///
/// const DEFAULT_CONFIG: &str = r#"
/// [server]
/// port = 2235
/// "#;
///
/// #[derive(Default, Deserialize)]
/// struct Config {
///     server: HttpServer,
/// }
///
/// impl Config {
///     fn server(&self) -> HttpServer {
///         self.server.clone()
///     }
/// }
///
/// async fn request() -> Response<Body> {
///     Response::new(Body::from("Hello world\n"))
/// }
///
/// type Connection = Tracked<TcpStream>;
///
/// fn main() {
///     let build_server =
///         |builder: Builder<_>, cfg: &HttpServer, name: &str, shutdown: Receiver<()>| {
///             eprintln!("Creating server {} for {:?}", name, cfg);
///             builder
///                 .serve(make_service_fn(|conn: &Connection| {
///                     let conn_addr = conn.peer_addr().expect("Peer address doesn't fail");
///                     eprintln!("New connection {}", conn_addr);
///                     async {
///                         Ok::<_, Infallible>(service_fn(|_req: Request<Body>| async {
///                             Ok::<_, Infallible>(request().await)
///                         }))
///                     }
///                 }))
///                 .with_graceful_shutdown(async {
///                     // Shutting down both by receiving a message and the other end being
///                     // dropped.
///                     let _ = shutdown.await;
///                 })
///         };
///     Spirit::<Empty, Config>::new()
///         .config_defaults(DEFAULT_CONFIG)
///         .with(
///             // Let's build a http server as configured by the user
///             Pipeline::new("listen")
///                 .extract_cfg(Config::server)
///                 // This is where we teach the server what it serves. It is the usual stuff from
///                 // hyper.
///                 .transform(BuildServer(build_server))
///                 .check()
///         )
///         .run(|spirit| {
/// #           let spirit = std::sync::Arc::clone(spirit);
/// #           std::thread::spawn(move || spirit.terminate());
///             Ok(())
///         });
/// }
/// ```
pub trait ServerBuilder<Tr>
where
    Tr: Fragment,
{
    /// The future returned by the build.
    ///
    /// The future shall represent the graceful shut down server.
    type OutputFut: Future<Output = Result<(), HyperError>> + Send;

    /// Invokes the build with the parameters.
    ///
    /// Directly corresponds to calling the closure for the blank implementation.
    fn build(
        &self,
        builder: Builder<Acceptor<Tr::Resource>>,
        cfg: &HyperServer<Tr>,
        name: &'static str,
        shutdown: Receiver<()>,
    ) -> Self::OutputFut;
}

impl<F, Tr, Fut> ServerBuilder<Tr> for F
where
    Tr: Fragment,
    F: Fn(Builder<Acceptor<Tr::Resource>>, &HyperServer<Tr>, &'static str, Receiver<()>) -> Fut,
    Fut: Future<Output = Result<(), HyperError>> + Send,
{
    type OutputFut = Fut;

    fn build(
        &self,
        builder: Builder<Acceptor<Tr::Resource>>,
        cfg: &HyperServer<Tr>,
        name: &'static str,
        shutdown: Receiver<()>,
    ) -> Fut {
        self(builder, cfg, name, shutdown)
    }
}

/// A simplified version of creating the [`ServerBuilder`].
///
/// Implementing the [`ServerBuilder`] by hand is possible and sometimes necessary (it is more
/// flexible ‒ one can receive parameters on the way ‒ the configuration of the specific server
/// instance in case there are multiple, getting access to the connection for which a service is
/// being created, unusual ways of creating the instances). But it is quite a chore with a lot of
/// `async` and `move` involved (you can have a look at source code of this function to get the
/// idea, the `[SRC]` button at the right).
///
/// This gets it done for the very simple case ‒ in case there's an async function/closure that
/// just takes a [`Request<Body>`][Request] and returns a [`Response<Body>`][Response].
///
/// See the [crate level][self] example.
pub fn server_from_handler<H, Tr, S>(handler: H) -> impl ServerBuilder<Tr> + Clone + Send
where
    Tr: Fragment,
    Tr::Resource: SpiritAccept + Unpin,
    <Tr::Resource as SpiritAccept>::Connection: AsyncRead + AsyncWrite + Unpin,
    H: Clone + Send + Sync + Fn(Request<Body>) -> S + 'static,
    S: Future<Output = Response<Body>> + Send + 'static,
{
    move |builder: Builder<Acceptor<Tr::Resource>>, _: &_, name, shutdown| {
        debug!("Creating server instance {}", name);
        let handler = handler.clone();
        builder
            .serve(make_service_fn(move |_conn| {
                trace!("Creating a service for {}", name);
                let handler = handler.clone();
                async move {
                    Ok::<_, Infallible>(service_fn(move |req| {
                        let handler = handler.clone();
                        async move { Ok::<_, Infallible>(handler(req).await) }
                    }))
                }
            }))
            .with_graceful_shutdown(async move {
                let _ = shutdown.await;
            })
    }
}
