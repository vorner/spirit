#![doc(
    html_root_url = "https://docs.rs/spirit-hyper/0.6.0/spirit_hyper/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

//! [Spirit] extension for Hyper
//!
//! This allows having Hyper servers auto-spawned from configuration. It is possible to put them on
//! top of arbitrary stream-style IO objects (TcpStream, UdsStream, these wrapped in SSL...).
//!
//! # Tokio runtime
//!
//! This uses the [`spirit-tokio`] crate under the hood. Similar drawback with initializing a
//! runtime applies here too (see the [`spirit-tokio`] docs for details).
//!
//! # Examples
//!
//! ```rust
//! use hyper::{Body, Request, Response};
//! use hyper::server::Builder;
//! use hyper::service::service_fn_ok;
//! use serde::Deserialize;
//! use spirit::{Empty, Pipeline, Spirit};
//! use spirit::prelude::*;
//! use spirit_hyper::{BuildServer, HttpServer};
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
//!         .with(
//!             // Let's build a http server as configured by the user
//!             Pipeline::new("listen")
//!                 .extract_cfg(Config::server)
//!                 // This is where we teach the server what it serves. It is the usual stuff from
//!                 // hyper.
//!                 .transform(BuildServer(|builder: Builder<_>, _cfg: &_, _name: &str| {
//!                     builder.serve(|| service_fn_ok(request))
//!                 }))
//!         )
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
//! [`spirit-tokio`]: spirit_tokio

use std::error::Error;
use std::fmt::Debug;
use std::io::Error as IoError;

use err_context::prelude::*;
use futures::sync::oneshot::{self, Receiver, Sender};
use futures::{Async, Future, Poll, Stream};
use hyper::body::Payload;
use hyper::server::{Builder, Server};
use hyper::service::{MakeServiceRef, Service};
use hyper::Body;
use log::debug;
use serde::{Deserialize, Serialize};
use spirit::fragment::driver::{CacheSimilar, Comparable, Comparison};
use spirit::fragment::{Fragment, Stackable, Transformation};
use spirit::AnyError;
use spirit::Empty;
use spirit_tokio::installer::FutureInstaller;
use spirit_tokio::net::limits::WithLimits;
use spirit_tokio::net::IntoIncoming;
use spirit_tokio::TcpListen;
#[cfg(feature = "cfg-help")]
use structdoc::StructDoc;
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[serde(rename_all = "kebab-case")]
struct HyperCfg {
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

    /// When a http1 client closes its write end, keep the connection open until the reply is sent.
    ///
    /// If set to false, if the client closes its connection, server does too.
    #[serde(default = "default_on")]
    http1_half_close: bool,

    #[serde(default)]
    http_mode: HttpMode,
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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[serde(rename_all = "kebab-case")]
pub struct HyperServer<Transport> {
    /// The inner transport.
    ///
    /// This is accessible by the user in case it contains something of use to the
    /// [`Transformation`]s.
    #[serde(flatten)]
    pub transport: Transport,

    #[serde(flatten)]
    inner: HyperCfg,
}

impl<Transport: Default> Default for HyperServer<Transport> {
    fn default() -> Self {
        HyperServer {
            transport: Transport::default(),
            inner: HyperCfg {
                http1_keepalive: true,
                http1_writev: true,
                http1_half_close: true,
                http_mode: HttpMode::default(),
            },
        }
    }
}

impl<Transport: Comparable> Comparable for HyperServer<Transport> {
    fn compare(&self, other: &Self) -> Comparison {
        let transport_cmp = self.transport.compare(&other.transport);
        if transport_cmp == Comparison::Same && self.inner != other.inner {
            Comparison::Similar
        } else {
            transport_cmp
        }
    }
}

impl<Transport> Fragment for HyperServer<Transport>
where
    Transport: Fragment + Debug + Clone + Comparable,
    Transport::Resource: IntoIncoming,
{
    type Driver = CacheSimilar<Self>;
    type Installer = ();
    type Seed = Transport::Seed;
    type Resource = Builder<<<Transport as Fragment>::Resource as IntoIncoming>::Incoming>;
    fn make_seed(&self, name: &'static str) -> Result<Self::Seed, AnyError> {
        self.transport.make_seed(name)
    }
    fn make_resource(
        &self,
        seed: &mut Self::Seed,
        name: &'static str,
    ) -> Result<Self::Resource, AnyError> {
        debug!("Creating HTTP server {}", name);
        let (h1_only, h2_only) = match self.inner.http_mode {
            HttpMode::Both => (false, false),
            HttpMode::Http1Only => (true, false),
            HttpMode::Http2Only => (false, true),
        };
        let transport = self.transport.make_resource(seed, name)?;
        let builder = Server::builder(transport.into_incoming())
            .http1_keepalive(self.inner.http1_keepalive)
            .http1_writev(self.inner.http1_writev)
            .http1_half_close(self.inner.http1_half_close)
            .http1_only(h1_only)
            .http2_only(h2_only);
        Ok(builder)
    }
}

impl<Transport> Stackable for HyperServer<Transport> where Transport: Stackable {}

/// A type alias for http (plain TCP) hyper server.
pub type HttpServer<ExtraCfg = Empty> = HyperServer<WithLimits<TcpListen<ExtraCfg>>>;

struct ActivateInner<Transport, MS> {
    server: Server<Transport, MS>,
    receiver: Receiver<()>,
}

/// A plumbing helper type.
///
/// This is public mostly because of Rust's current lack of existential types in traits. It is
/// basically a `impl Future<Item = (), Error = ()>`, but as a concrete type.
///
/// The user should not need to interact directly with this.
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
    MS::Error: Into<Box<dyn Error + Send + Sync>>,
    MS::Future: Send + 'static,
    <MS::Future as Future>::Error: Error + Send + Sync,
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
                    let e = e.context(format!("HTTP server {} failed", name));
                    spirit::log_error!(multi Error, e.into());
                });
            tokio::spawn(server);
        }
        Ok(Async::NotReady)
    }
}

/// A [`Transformation`] to turn a [`Builder`] into a [`Server`].
///
/// The value wrapped in this shall be a closure taking:
/// * The [`Builder`]
/// * The configuration fragment ([`HyperServer`])
/// * A `&str` name
///
/// It shall produce a [`Server`]. This is usually done through the [`serve`][Server::serve]
/// method. It also pairs the resource with an [`Installer`][spirit::fragment::Installer].
///
/// Note that a graceful shutdown of the [`Server`] is done as part of the automatic plumbing.
pub struct BuildServer<BS>(pub BS);

impl<Transport, Inst, BS, Incoming, S, B>
    Transformation<Builder<Incoming>, Inst, HyperServer<Transport>> for BuildServer<BS>
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
    fn transform(
        &mut self,
        builder: Builder<Incoming>,
        cfg: &HyperServer<Transport>,
        name: &'static str,
    ) -> Result<Self::OutputResource, AnyError> {
        let (sender, receiver) = oneshot::channel();
        let server = self.0(builder, cfg, name);
        Ok(Activate {
            inner: Some(ActivateInner { server, receiver }),
            sender: Some(sender),
            name,
        })
    }
}
