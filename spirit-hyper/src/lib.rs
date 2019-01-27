#![doc(
    html_root_url = "https://docs.rs/spirit-hyper/0.4.1/spirit_hyper/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! [Spirit] helper for Hyper
//!
//! This allows having Hyper servers auto-spawned from configuration. It is possible to put them on
//! top of arbitrary stream-style IO objects (TcpStream, UdsStream, these wrapped in SSL...).
//!
//! # Examples
//!
//! ```rust
//! extern crate hyper;
//! extern crate serde;
//! #[macro_use]
//! extern crate serde_derive;
//! extern crate spirit;
//! extern crate spirit_hyper;
//! extern crate spirit_tokio;
//!
//! use hyper::{Body, Request, Response};
//! use spirit::{Empty, Spirit};
//! use spirit_hyper::HttpServer;
//!
//! const DEFAULT_CONFIG: &str = r#"
//! [server]
//! port = 1234
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
//! fn request(_req: Request<Body>) -> Response<Body> {
//!     Response::new(Body::from("Hello world\n"))
//! }
//!
//! fn main() {
//!     Spirit::<Empty, Config>::new()
//!         .config_defaults(DEFAULT_CONFIG)
//!         .config_helper(Config::server, spirit_hyper::server_ok(request), "server")
//!         .run(|spirit| {
//! #           let spirit = std::sync::Arc::clone(spirit);
//! #           std::thread::spawn(move || spirit.terminate());
//!             Ok(())
//!         });
//! }
//! ```
//!
//! Further examples are in the
//! [git repository](https://github.com/vorner/spirit/tree/master/spirit-hyper/examples).
//!
//! [Spirit]: https://crates.io/crates/spirit.

extern crate failure;
extern crate futures;
extern crate hyper;
#[macro_use]
extern crate log;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate spirit;
extern crate spirit_tokio;
#[cfg(feature = "cfg-help")]
#[macro_use]
extern crate structdoc;
extern crate tokio;

use std::error::Error as EError;
use std::io::Error as IoError;

use failure::{Error, Fail};
use futures::sync::oneshot::{self, Receiver, Sender};
use futures::{Async, Future, Poll, Stream};
use hyper::body::Payload;
use hyper::server::{Builder, Server};
use hyper::service::{MakeServiceRef, Service};
use hyper::Body;
use spirit::Empty;
use spirit::fragment::driver::TrivialDriver;
use spirit::fragment::{Fragment, Stackable, Transformation};
use spirit_tokio::installer::FutureInstaller;
use spirit_tokio::net::limits::WithLimits;
use spirit_tokio::net::IntoIncoming;
use spirit_tokio::TcpListen;
use tokio::io::{AsyncRead, AsyncWrite};

fn default_on() -> bool {
    true
}

#[derive(Copy, Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[serde(rename_all = "kebab-case")]
enum HttpMode {
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

#[derive(
    Copy, Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize,
)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[serde(rename_all = "kebab-case")]
struct HttpModeWorkaround {
    /// What HTTP mode (protocols) to support.
    #[serde(default)]
    http_mode: HttpMode,
}

/// A [`ResourceConfig`] for hyper servers.
///
/// This is a wrapper around a `Transport` [`ResourceConfig`]. It takes something that accepts
/// connections ‒ like [`TcpListen`] and adds configuration specific for HTTP server.
///
/// This can then be paired with one of the [`ResourceConsumer`]s created by `server` functions to
/// spawn servers:
///
/// * [`server`]
/// * [`server_configured`]
/// * [`server_simple`]
/// * [`server_ok`]
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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[serde(rename_all = "kebab-case")]
pub struct HyperServer<Transport> {
    #[serde(flatten)]
    pub transport: Transport,

    /// The HTTP keepalive.
    ///
    /// https://en.wikipedia.org/wiki/HTTP_persistent_connection.
    ///
    /// Default is on, can be turned off.
    #[serde(default = "default_on")]
    http1_keepalive: bool,

    /// Vectored writes of headers.
    ///
    /// This is a low-level optimization setting. Using the vectored writes saves some copying of
    /// data around, but can be slower on some systems or transports.
    ///
    /// Default is on, can be turned off.
    #[serde(default = "default_on")]
    http1_writev: bool,

    #[serde(default, flatten)]
    http_mode: HttpModeWorkaround,
}

impl<Transport: Default> Default for HyperServer<Transport> {
    fn default() -> Self {
        HyperServer {
            transport: Transport::default(),
            http1_keepalive: true,
            http1_writev: true,
            http_mode: HttpModeWorkaround {
                http_mode: HttpMode::Both,
            },
        }
    }
}

impl<Transport> Fragment for HyperServer<Transport>
where
    Transport: Fragment,
    Transport::Resource: IntoIncoming,
{
    type Driver = TrivialDriver; // XXX
    type Installer = ();
    type Seed = Transport::Seed;
    type Resource = Builder<<<Transport as Fragment>::Resource as IntoIncoming>::Incoming>;
    fn make_seed(&self, name: &'static str) -> Result<Self::Seed, Error> {
        self.transport.make_seed(name)
    }
    fn make_resource(&self, seed: &mut Self::Seed, name: &'static str)
        -> Result<Self::Resource, Error>
    {
        debug!("Creating HTTP server {}", name);
        let (h1_only, h2_only) = match self.http_mode.http_mode {
            HttpMode::Both => (false, false),
            HttpMode::Http1Only => (true, false),
            HttpMode::Http2Only => (false, true),
        };
        let transport = self.transport.make_resource(seed, name)?;
        let builder = Server::builder(transport.into_incoming())
            .http1_keepalive(self.http1_keepalive)
            .http1_writev(self.http1_writev)
            .http1_only(h1_only)
            .http2_only(h2_only);
        Ok(builder)
    }
}

impl<Transport> Stackable for HyperServer<Transport>
where
    Transport: Stackable
{}

/// A type alias for http (plain TCP) hyper server.
pub type HttpServer<ExtraCfg = Empty> = HyperServer<WithLimits<TcpListen<ExtraCfg>>>;

struct ActivateInner<Transport, MS> {
    server: Server<Transport, MS>,
    receiver: Receiver<()>,
}

pub struct Activate<Transport, MS> {
    inner: Option<ActivateInner<Transport, MS>>,
    sender: Option<Sender<()>>,
    name: &'static str,
}

impl<Transport, MS> Drop for Activate<Transport, MS> {
    fn drop(&mut self) {
        // Tell the server to terminate
        let _ = self.sender.take().expect("Dropped multiple times").send(());
    }
}

impl<Transport, MS, B> Future for Activate<Transport, MS>
where
    Transport: Stream<Error = IoError> + Send + Sync + 'static,
    Transport::Item: AsyncRead + AsyncWrite + Send + Sync,
    MS: MakeServiceRef<Transport::Item, ReqBody = Body, ResBody = B> + Send + 'static,
    MS::Error: Into<Box<dyn EError + Send + Sync>>,
    MS::Future: Send + 'static,
    <MS::Future as Future>::Error: EError + Send + Sync,
    MS::Service: Send + 'static,
    <MS::Service as Service>::Future: Send,
    B: Payload,
{
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<(), ()> {
        if let Some(inner) = self.inner.take() {
            let name = self.name;
            let server = inner
                .server
                .with_graceful_shutdown(inner.receiver)
                .map_err(move |e| {
                    spirit::log_error(module_path!(), &e.context(format!("HTTP server {} failed", name)).into());
                });
            tokio::spawn(server);
        }
        Ok(Async::NotReady)
    }
}

pub struct BuildServer<BS>(pub BS);

impl<Transport, Inst, BS, Incoming, S, B> Transformation<Builder<Incoming>, Inst, HyperServer<Transport>> for BuildServer<BS>
where
    Transport: Fragment + 'static,
    Transport::Resource: IntoIncoming<Incoming = Incoming, Connection = Incoming::Item>,
    Incoming: Stream<Error = IoError> + Send + Sync + 'static,
    Incoming::Item: AsyncRead + AsyncWrite + Send + Sync + 'static,
    BS: Fn(Builder<Incoming>, &HyperServer<Transport>, &'static str) -> Server<Incoming, S>,
    S: MakeServiceRef<Incoming::Item, ReqBody = Body, ResBody = B> + 'static,
    B: Payload,
{
    type OutputResource = Activate<Incoming, S>;
    type OutputInstaller = FutureInstaller<Self::OutputResource>;
    fn installer(&mut self, _ii: Inst, _name: &'static str) -> Self::OutputInstaller {
        FutureInstaller::default()
    }
    fn transform(&mut self, builder: Builder<Incoming>, cfg: &HyperServer<Transport>, name: &'static str) -> Result<Self::OutputResource, Error> {
        let (sender, receiver) = oneshot::channel();
        let server = self.0(builder, cfg, name);
        Ok(Activate {
            inner: Some(ActivateInner {
                server,
                receiver,
            }),
            sender: Some(sender),
            name,
        })
    }
}
