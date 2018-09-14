#![doc(
    html_root_url = "https://docs.rs/spirit-hyper/0.1.0/spirit_hyper/",
    test(attr(deny(warnings)))
)]
#![cfg_attr(feature = "cargo-clippy", allow(type_complexity))]
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
use std::fmt::{Debug, Display};
use std::sync::Arc;

use arc_swap::ArcSwap;
use failure::Error;
use futures::{Future, IntoFuture};
use hyper::server::conn::Http;
use hyper::service::Service;
use hyper::Body;
use serde::Deserialize;
use spirit::helpers::IteratedCfgHelper;
use spirit::{Builder, Spirit};
use spirit_tokio::ResourceMaker;
use structopt::StructOpt;
use tokio_io::{AsyncRead, AsyncWrite};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
struct HyperServer<Transport> {
    #[serde(flatten)]
    transport: Transport,
}

// TODO: default_http something helper

impl<S, O, C, Transport, Action, ActionFut, Srv, H> IteratedCfgHelper<S, O, C, Action>
    for HyperServer<Transport>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    Transport: ResourceMaker<S, O, C>,
    Transport::Resource: AsyncRead + AsyncWrite + Send + 'static,
    Action: Fn(&Arc<Spirit<S, O, C>>, &Transport::ExtraCfg) -> ActionFut + Sync + Send + 'static,
    ActionFut: IntoFuture<Item = (Srv, H), Error = Error>,
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
        let inner_action = move |spirit: &_, resource, extra_cfg: &_| {
            action(spirit, extra_cfg)
                .into_future()
                .and_then(|(srv, http)| {
                    http.borrow()
                        .serve_connection(resource, srv)
                        .map_err(Error::from)
                })
        };
        let inner_extractor = move |cfg: &_| {
            extractor(cfg)
                .into_iter()
                .map(|instance| instance.transport)
        };
        Transport::apply(inner_extractor, inner_action, name, builder)
    }
}
