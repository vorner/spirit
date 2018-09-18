#![doc(
    html_root_url = "https://docs.rs/spirit-hyper/0.1.0/spirit_hyper/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate arc_swap;
extern crate failure;
extern crate futures;
extern crate hyper;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate spirit;
extern crate spirit_tokio;
extern crate structopt;
extern crate tokio_io;

use std::borrow::Borrow;
use std::error::Error;
use std::fmt::{Debug, Display};
use std::sync::Arc;

use arc_swap::ArcSwap;
use failure::Error as FailError;
use futures::future::Shared;
use futures::sync::oneshot::{self, Receiver};
use futures::{Async, Future, IntoFuture, Poll};
use hyper::body::Payload;
use hyper::server::conn::{Connection, Http};
use hyper::service::Service;
use hyper::Body;
use serde::Deserialize;
use spirit::helpers::IteratedCfgHelper;
use spirit::{Builder, Empty, Spirit};
use spirit_tokio::{ResourceMaker, TcpListen};
use structopt::StructOpt;
use tokio_io::{AsyncRead, AsyncWrite};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct HyperServer<Transport> {
    #[serde(flatten)]
    transport: Transport,
}

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
        let do_shutdown = self.shutdown
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

impl<S, O, C, Transport, Action, ActionFut, Srv, H> IteratedCfgHelper<S, O, C, Action>
    for HyperServer<Transport>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    Transport: ResourceMaker<S, O, C, ()>,
    Transport::Resource: AsyncRead + AsyncWrite + Send + 'static,
    Action: Fn(&Arc<Spirit<S, O, C>>, &Transport::ExtraCfg) -> ActionFut + Sync + Send + 'static,
    ActionFut: IntoFuture<Item = (Srv, H), Error = FailError>,
    ActionFut::Future: Send + 'static,
    Srv: Service<ReqBody = Body> + Send + 'static,
    Srv::Future: Send,
    H: Borrow<Http> + Send + 'static,
{
    fn apply<Extractor, ExtractedIter, Name>(
        mut extractor: Extractor,
        action: Action,
        name: Name,
        builder: Builder<S, O, C>,
    ) -> Builder<S, O, C>
    where
        Extractor: FnMut(&C) -> ExtractedIter + Send + 'static,
        ExtractedIter: IntoIterator<Item = Self>,
        Name: Clone + Display + Send + Sync + 'static,
    {
        let (shutdown_send, shutdown_recv) = oneshot::channel::<()>();
        let shutdown_recv = shutdown_recv.shared();
        let inner_action = move |spirit: &_, resource, extra_cfg: &_, _: &()| {
            let shutdown_recv = shutdown_recv.clone();
            action(spirit, extra_cfg)
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
