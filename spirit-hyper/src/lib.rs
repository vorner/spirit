#![doc(
    html_root_url = "https://docs.rs/spirit-hyper/0.2.0/spirit_hyper/",
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
//! #![type_length_limit="8388608"]
//! extern crate failure;
//! extern crate hyper;
//! extern crate serde;
//! #[macro_use]
//! extern crate serde_derive;
//! extern crate spirit;
//! extern crate spirit_hyper;
//!
//! use std::sync::Arc;
//!
//! use hyper::{Body, Request, Response};
//! use spirit::{Empty, Spirit};
//! use spirit_hyper::HttpServer;
//!
//! const DEFAULT_CONFIG: &str = r#"
//! [server]
//! port = 1234
//! "#;
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
//! fn request(_: &Arc<Spirit<Empty, Config>>, _req: Request<Body>, _: &Empty) -> Response<Body> {
//!     Response::new(Body::from("Hello world\n"))
//! }
//!
//! fn main() {
//!     Spirit::<Empty, Config>::new()
//!         .config_defaults(DEFAULT_CONFIG)
//!         .config_helper(Config::server, spirit_hyper::service_fn_ok(request), "Server")
//!         .run(|spirit| {
//! #           let spirit = Arc::clone(spirit);
//! #           std::thread::spawn(move || spirit.terminate());
//!             Ok(())
//!         });
//! }
//! ```
//! Further examples are in the
//! [git repository](https://github.com/vorner/spirit/tree/master/spirit-hyper/examples).
//!
//! # Known problems
//!
//! * Not many helper generators are present ‒ only [`service_fn_ok`](fn.service_fn_ok.html) for
//!   now. And that one doesn't (yet) support futures.
//! * It creates some huge types under the hood. For now, if the compiler complains about
//!   `type_length_limit`, try increasing it (maybe even multiple times). This might be overcome in
//!   the future, but for now, the main downside to it is probably compile times.
//!
//! [Spirit]: https://crates.io/crates/spirit.

extern crate arc_swap;
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
extern crate structopt;
extern crate tokio;

use std::borrow::Borrow;
use std::error::Error;
use std::fmt::{Debug, Display};
use std::iter;
use std::sync::Arc;

use failure::Error as FailError;
use futures::future::Shared;
use futures::sync::oneshot::{self, Receiver, Sender};
use futures::{Async, Future, IntoFuture, Poll};
use hyper::body::Payload;
use hyper::server::Server;
use hyper::server::conn::{Connection, Http};
use hyper::service::{self, MakeService, Service};
use hyper::{Body, Request, Response};
use serde::de::DeserializeOwned;
use spirit::helpers::{CfgHelper, IteratedCfgHelper};
use spirit::{Builder, Empty, Spirit};
use spirit::validation::Results as ValidationResults;
use spirit_tokio::{ResourceMaker, TcpListen};
use spirit_tokio::{ExtraCfgCarrier, Name, IntoIncoming, ResourceConfig, ResourceConsumer};
use structopt::StructOpt;
use tokio::io::{AsyncRead, AsyncWrite};

struct SendOnDrop(Option<Sender<()>>);

impl Drop for SendOnDrop {
    fn drop(&mut self) {
        let _ = self.0.take().unwrap().send(());
    }
}

impl Future for SendOnDrop {
    type Item = ();
    type Error = FailError;
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        Ok(Async::NotReady)
    }
}

pub trait ConfiguredMakeService<O, C, Cfg>: Send + Sync + 'static
where
    Cfg: ResourceConfig<O, C>,
{
    type MakeService;
    fn make(&self, spirit: &Arc<Spirit<O, C>>, cfg: &Arc<Cfg>, resource: &Cfg::Resource, name: &str) -> Self::MakeService;
}

impl<O, C, Cfg, F, R> ConfiguredMakeService<O, C, Cfg> for F
where
    Cfg: ResourceConfig<O, C>,
    F: Fn(&Arc<Spirit<O, C>>, &Arc<Cfg>, &Cfg::Resource, &str) -> R + Send + Sync + 'static,
{
    type MakeService = R;
    fn make(&self, spirit: &Arc<Spirit<O, C>>, cfg: &Arc<Cfg>, resource: &Cfg::Resource, name: &str) -> R {
        self(spirit, cfg, resource, name)
    }
}

pub fn server<R, O, C, CMS, B, E, ME, S, F>(configured_make_service: CMS)
    -> impl ResourceConsumer<HyperServer<R>, O, C>
where
    R: ResourceConfig<O, C>,
    R::Resource: IntoIncoming,
    <R::Resource as IntoIncoming>::Connection: AsyncRead + AsyncWrite,
    CMS: ConfiguredMakeService<O, C, HyperServer<R>>,
    // TODO: Ask hyper to make their MakeServiceRef public, this monster is ugly :-(.
    CMS::MakeService: for<'a> MakeService<&'a <R::Resource as IntoIncoming>::Connection, ReqBody = Body, Error = E, MakeError = ME, Service = S, Future = F, ResBody = B> + Send + Sync + 'static,
    E: Into<Box<Error + Send + Sync>>,
    ME: Into<Box<Error + Send + Sync>>,
    S: Service<ReqBody=Body, ResBody=B, Error=E> + Send + 'static,
    S::Future: Send,
    F: Future<Item=S, Error=ME> + Send + 'static,
    B: Payload,
{
    move |spirit: &Arc<Spirit<O, C>>, config: &Arc<HyperServer<R>>, resource: R::Resource, name: &str| {
        let (sender, receiver) = oneshot::channel();
        debug!("Starting hyper server {}", name);
        let name_success = name.to_owned();
        let name_err = name.to_owned();
        let make_service = configured_make_service.make(spirit, config, &resource, name);
        let server = Server::builder(resource.into_incoming())
            .serve(make_service)
            .with_graceful_shutdown(receiver)
            .map(move |()| debug!("Hyper server {} shut down", name_success))
            .map_err(move |e| error!("Hyper server {} failed: {}", name_err, e));
        tokio::spawn(server);
        SendOnDrop(Some(sender))
    }
}

pub fn server_ok<R, O, C, S, B>(service: S) -> impl ResourceConsumer<HyperServer<R>, O, C>
where
    R: ResourceConfig<O, C>,
    R::Resource: IntoIncoming,
    <R::Resource as IntoIncoming>::Connection: AsyncRead + AsyncWrite,
    S: Fn(Request<Body>) -> Response<B> + Clone + Send + Sync + 'static,
    B: Payload,
{
    let configure_service = move |_: &_, _: &_, _: &_, _: &_| {
        let service = service.clone();
        move || hyper::service::service_fn_ok(service.clone())
    };
    server(configure_service)
}

pub fn server_simple<R, O, C, S, Fut, B>(service: S) -> impl ResourceConsumer<HyperServer<R>, O, C>
where
    R: ResourceConfig<O, C>,
    R::Resource: IntoIncoming,
    <R::Resource as IntoIncoming>::Connection: AsyncRead + AsyncWrite,
    S: Fn(Request<Body>) -> Fut + Clone + Send + Sync + 'static,
    Fut: IntoFuture<Item = Response<B>> + Send + 'static,
    Fut::Future: Send + 'static,
    Fut::Error: Into<Box<Error + Send + Sync>>,
    B: Payload,
{
    let configure_service = move |_: &_, _: &_, _: &_, _: &_| {
        let service = service.clone();
        move || hyper::service::service_fn(service.clone())
    };
    server(configure_service)
}

pub fn server_configured<R, O, C, S, Fut, B>(service: S) -> impl ResourceConsumer<HyperServer<R>, O, C>
where
    C: Send + Sync + 'static,
    O: Send + Sync + 'static,
    R: ResourceConfig<O, C>,
    R::Resource: IntoIncoming,
    <R::Resource as IntoIncoming>::Connection: AsyncRead + AsyncWrite,
    S: Fn(&Arc<Spirit<O, C>>, &Arc<HyperServer<R>>, Request<Body>) -> Fut + Clone + Send + Sync + 'static,
    Fut: IntoFuture<Item = Response<B>> + Send + 'static,
    Fut::Future: Send + 'static,
    Fut::Error: Into<Box<Error + Send + Sync>>,
    B: Payload,
{
    let configure_service = move |spirit: &_, cfg: &_, _: &_, _: &_| {
        let service = service.clone();
        let spirit = Arc::clone(spirit);
        let cfg = Arc::clone(cfg);
        move || {
            let service = service.clone();
            let spirit = Arc::clone(&spirit);
            let cfg = Arc::clone(&cfg);
            hyper::service::service_fn(move |req| service(&spirit, &cfg, req))
        }
    };
    server(configure_service)
}

/// A configuration fragment that can spawn Hyper server.
///
/// This is `CfgHelper` and `IteratedCfgHelper` ‒ you can use either singular or a container as the
/// configuration fragment and pair it with a request handler.
///
/// The `Transport` type parameter is subfragment that describes the listening socket or something
/// similar ‒ for example `spirit_tokio::TcpListen`.
///
/// The request handler must implement the [`ConnAction`](trait.ConnAction.html) trait.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct HyperServer<Transport> {
    #[serde(flatten)]
    transport: Transport,
}

impl<T> ExtraCfgCarrier for HyperServer<T>
where
    T: ExtraCfgCarrier,
{
    type Extra = T::Extra;
    fn extra(&self) -> &T::Extra {
        self.transport.extra()
    }
}

impl<O, C, T> ResourceConfig<O, C> for HyperServer<T>
where
    T: ResourceConfig<O, C>,
{
    type Seed = T::Seed;
    type Resource = T::Resource;
    fn create(&self, name: &str) -> Result<Self::Seed, FailError> {
        self.transport.create(name)
    }
    fn fork(&self, seed: &Self::Seed, name: &str) -> Result<Self::Resource, FailError> {
        self.transport.fork(seed, name)
    }
    fn scaled(&self, name: &str) -> (usize, ValidationResults) {
        self.transport.scaled(name)
    }
    fn is_similar(&self, other: &Self, name: &str) -> bool {
        self.transport.is_similar(&other.transport, name)
    }
    fn install<N: Name>(builder: Builder<O, C>, name: &N) -> Builder<O, C> {
        T::install(builder, name)
    }
}

/// A type alias for http (plain TCP) hyper server.
pub type HttpServer<ExtraCfg = Empty> = HyperServer<TcpListen<ExtraCfg>>;

struct ShutdownConn<T, S: Service> {
    conn: Connection<T, S>,
    shutdown: Option<Shared<Receiver<()>>>,
}

impl<T, S, B> Future for ShutdownConn<T, S>
where
    S: Service<ReqBody = Body, ResBody = B> + 'static,
    S::Error: Into<Box<Error + Send + Sync>>,
    S::Future: Send,
    T: AsyncRead + AsyncWrite + 'static,
    B: Payload + 'static,
{
    type Item = <Connection<T, S> as Future>::Item;
    type Error = <Connection<T, S> as Future>::Error;
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let do_shutdown = self
            .shutdown
            .as_mut()
            .map(Future::poll)
            .unwrap_or(Ok(Async::NotReady));
        match do_shutdown {
            Ok(Async::NotReady) => (), // Don't shutdown yet (or already done that)
            _ => {
                self.conn.graceful_shutdown();
                self.shutdown.take();
            }
        }
        self.conn.poll()
    }
}

/// A trait describing the connection action.
///
/// This is usually a function
/// `Fn(&Arc<Spirit>, &ExtraCfg) -> impl IntoFuture<Item = (impl Service, Http), Error>`
///
/// It is similar to `hyper::NewService`, but with extra access to the spirit and extra config from
/// the transport fragment.
pub trait ConnAction<O, C, ExtraCfg> {
    /// The type the function-like thing returns.
    type IntoFuture;

    /// Performs the action.
    ///
    /// This should create the `hyper::Service` used to handle a connection.
    fn action(&self, &Arc<Spirit<O, C>>, &ExtraCfg) -> Self::IntoFuture;
}

impl<F, O, C, ExtraCfg, R> ConnAction<O, C, ExtraCfg> for F
where
    F: Fn(&Arc<Spirit<O, C>>, &ExtraCfg) -> R,
{
    type IntoFuture = R;
    fn action(&self, arc: &Arc<Spirit<O, C>>, extra: &ExtraCfg) -> R {
        self(arc, extra)
    }
}

/// A helper to create a [`ConnAction`](trait.ConnAction.html) from a function (or closure).
///
/// This turns a `Fn(&Arc<Spirit>, &Request<Body>, &ExtraCfg) -> Response<Body>` into a
/// `ConnAction`, so it can be fed into `cfg_helper`.
pub fn service_fn_ok<F, O, C, ExtraCfg>(
    f: F,
) -> impl ConnAction<
    O,
    C,
    ExtraCfg,
    IntoFuture = Result<
        (
            impl Service<ReqBody = Body, Future = impl Send> + Send,
            Http,
        ),
        FailError,
    >,
>
where
    // TODO: Make more generic ‒ return future, payload, ...
    F: Fn(&Arc<Spirit<O, C>>, Request<Body>, &ExtraCfg) -> Response<Body> + Send + Sync + 'static,
    ExtraCfg: Clone + Debug + PartialEq + Send + 'static,
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
{
    let f = Arc::new(f);
    move |spirit: &_, extra_cfg: &ExtraCfg| -> Result<_, FailError> {
        let spirit = Arc::clone(spirit);
        let extra_cfg = extra_cfg.clone();
        let f = Arc::clone(&f);
        let svc = move |req: Request<Body>| -> Response<Body> { f(&spirit, req, &extra_cfg) };
        Ok((service::service_fn_ok(svc), Http::new()))
    }
}

// TODO: implement service_fn

impl<O, C, Transport, Action, Srv, H> IteratedCfgHelper<O, C, Action> for HyperServer<Transport>
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    Transport: ResourceMaker<O, C, ()>,
    Transport::Resource: AsyncRead + AsyncWrite + Send + 'static,
    Action: ConnAction<O, C, Transport::ExtraCfg> + Sync + Send + 'static,
    Action::IntoFuture: IntoFuture<Item = (Srv, H), Error = FailError>,
    <Action::IntoFuture as IntoFuture>::Future: Send + 'static,
    Srv: Service<ReqBody = Body> + Send + 'static,
    Srv::Future: Send,
    H: Borrow<Http> + Send + 'static,
{
    fn apply<Extractor, ExtractedIter, Name>(
        mut extractor: Extractor,
        action: Action,
        name: Name,
        builder: Builder<O, C>,
    ) -> Builder<O, C>
    where
        Extractor: FnMut(&C) -> ExtractedIter + Send + 'static,
        ExtractedIter: IntoIterator<Item = Self>,
        Name: Clone + Display + Send + Sync + 'static,
    {
        let (shutdown_send, shutdown_recv) = oneshot::channel::<()>();
        let shutdown_recv = shutdown_recv.shared();
        let inner_action = move |spirit: &_, resource, extra_cfg: &_, _: &()| {
            let shutdown_recv = shutdown_recv.clone();
            action
                .action(spirit, extra_cfg)
                .into_future()
                .and_then(|(srv, http)| {
                    let conn = http.borrow().serve_connection(resource, srv);
                    let conn = ShutdownConn {
                        shutdown: Some(shutdown_recv),
                        conn,
                    };
                    conn.map_err(FailError::from)
                })
        };
        let inner_extractor = move |cfg: &_| {
            extractor(cfg)
                .into_iter()
                .map(|instance| (instance.transport, ()))
        };
        let mut shutdown_send = Some(shutdown_send);
        Transport::apply(inner_extractor, inner_action, name, builder).on_terminate(move || {
            if let Some(send) = shutdown_send.take() {
                let _ = send.send(());
            }
        })
    }
}

impl<O, C, Transport, Action, Srv, H> CfgHelper<O, C, Action> for HyperServer<Transport>
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    Transport: ResourceMaker<O, C, ()>,
    Transport::Resource: AsyncRead + AsyncWrite + Send + 'static,
    Action: ConnAction<O, C, Transport::ExtraCfg> + Sync + Send + 'static,
    Action::IntoFuture: IntoFuture<Item = (Srv, H), Error = FailError>,
    <Action::IntoFuture as IntoFuture>::Future: Send + 'static,
    Srv: Service<ReqBody = Body> + Send + 'static,
    Srv::Future: Send,
    H: Borrow<Http> + Send + 'static,
{
    fn apply<Extractor, Name>(
        mut extractor: Extractor,
        action: Action,
        name: Name,
        builder: Builder<O, C>,
    ) -> Builder<O, C>
    where
        Extractor: FnMut(&C) -> Self + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        let extractor = move |cfg: &_| iter::once(extractor(cfg));
        <Self as IteratedCfgHelper<O, C, Action>>::apply(extractor, action, name, builder)
    }
}
