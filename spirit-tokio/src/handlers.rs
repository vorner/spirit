//! Handlers of connections and sockets.
//!
//! The [`Fragment`]s provided by this crate are not directly useful on their own. Unlike for
//! example logging, a listening TCP socket doesn't *do* anything on its own.
//!
//! This module provides [`Transformation`]s that bind the sockets with some useful action
//! (different handlers represent different abstractions from the actions). In general, these
//! actions should return a [`Future`] and that one can in turn be installed.
//!
//! See the [crate example](../index.html#examples) for the usage.
//!
//! [`Fragment`]: spirit::Fragment
//! [`Transformation`]: spirit::fragment::Transformation
//! [`Future`]: futures::Future
use std::fmt::Debug;
use std::io::Error as IoError;

use failure::{Error, Fail};
use futures::{try_ready, Async, Future, IntoFuture, Poll, Stream};
use log::{trace, warn};
use spirit::fragment::Transformation;

use crate::installer::FutureInstaller;
use crate::net::IntoIncoming;

/// A [`Transformation`] to handle the whole socket.
///
/// This is a newtype that turns a closure taking a socket and the [`Fragment`] describing it and
/// returns a future of something that can be done with it.
///
/// This is appropriate for UDP-like sockets and if you want to accept the connections on a
/// TCP-like socket manually.
///
/// If you want to handle separate connections from a TCP listening socket but don't want to care
/// about accepting them (and want to leave some other bookkeeping, like the limits of connections,
/// to this crate), then you want the [`HandleListener`] instead.
///
/// [`Fragment`]: spirit::Fragment
#[derive(Clone, Debug)]
pub struct HandleSocket<F>(pub F);

impl<Socket, InputInstaller, SubFragment, F, R> Transformation<Socket, InputInstaller, SubFragment>
    for HandleSocket<F>
where
    F: FnMut(Socket, &SubFragment) -> Result<R, Error>,
    R: IntoFuture<Item = (), Error = ()> + 'static,
    SubFragment: Debug,
{
    type OutputResource = R;
    type OutputInstaller = FutureInstaller<R>;
    fn installer(&mut self, _: InputInstaller, name: &str) -> Self::OutputInstaller {
        trace!("Creating future installer for {}", name);
        FutureInstaller::default()
    }
    fn transform(&mut self, sock: Socket, cfg: &SubFragment, name: &str) -> Result<R, Error> {
        trace!("Creating a future out of socket {} on {:?}", name, cfg);
        (self.0)(sock, cfg)
    }
}

#[doc(hidden)]
pub trait ConnectionHandler<Conn, Ctx> {
    type Output;
    fn execute(&self, conn: Conn, ctx: &mut Ctx) -> Self::Output;
}

impl<F, Conn, Ctx, R> ConnectionHandler<Conn, Ctx> for F
where
    F: Fn(Conn, &mut Ctx) -> R,
{
    type Output = R;
    fn execute(&self, conn: Conn, ctx: &mut Ctx) -> R {
        self(conn, ctx)
    }
}

#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct ConfigAdaptor<F>(F);

impl<F, Conn, Cfg, R> ConnectionHandler<Conn, Cfg> for ConfigAdaptor<F>
where
    F: Fn(Conn, &Cfg) -> R,
{
    type Output = R;
    fn execute(&self, conn: Conn, cfg: &mut Cfg) -> R {
        (self.0)(conn, cfg)
    }
}

#[doc(hidden)]
pub struct Acceptor<Incoming, Ctx, Handler> {
    name: &'static str,
    incoming: Incoming,
    ctx: Ctx,
    handler: Handler,
}

impl<Incoming, Ctx, Handler> Future for Acceptor<Incoming, Ctx, Handler>
where
    Incoming: Stream<Error = IoError>,
    Handler: ConnectionHandler<Incoming::Item, Ctx>,
    Handler::Output: IntoFuture<Item = ()>,
    <<Handler as ConnectionHandler<Incoming::Item, Ctx>>::Output as IntoFuture>::Future:
        Send + 'static,
    <<Handler as ConnectionHandler<Incoming::Item, Ctx>>::Output as IntoFuture>::Error: Into<Error>,
{
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<(), ()> {
        loop {
            let incoming = self.incoming.poll().map_err(|e| {
                let e = e.context("Listening socket terminated unexpectedly").into();
                spirit::log_error!(multi Error, e);
            });
            let connection = try_ready!(incoming);
            if let Some(conn) = connection {
                let name = self.name;
                let future = self
                    .handler
                    .execute(conn, &mut self.ctx)
                    .into_future()
                    .map_err(move |e| {
                        let e = e
                            .into()
                            .context(format!("Failed to handle connection on {}", name));
                        spirit::log_error!(multi Error, e.into());
                    });
                tokio::spawn(future);
            } else {
                warn!("The listening socket on {} terminated", self.name);
                return Ok(Async::Ready(()));
            }
        }
    }
}

/// A handler of incoming connections with per-listener initialization.
///
/// This is a more complex version of the [`HandleListener`]. This one contains two closures. The
/// first one (`I`) is called for each *listening* socket and produces an arbitrary context. The
/// second one (`F`) is called for each connection accepted on that listening socket. The context
/// is provided to the closure.
///
/// This approach allows certain initialization to happen for each separate listening socket.
#[derive(Clone, Debug)]
pub struct HandleListenerInit<I, F>(pub I, pub F);

impl<Listener, InputInstaller, SubFragment, I, Ctx, F, Fut>
    Transformation<Listener, InputInstaller, SubFragment> for HandleListenerInit<I, F>
where
    Listener: IntoIncoming,
    I: FnMut(&mut Listener, &SubFragment) -> Result<Ctx, Error>,
    F: Fn(Listener::Connection, &mut Ctx) -> Fut + Clone + 'static,
    Fut: IntoFuture<Item = ()>,
    Fut::Error: Into<Error>,
    SubFragment: Debug,
    Ctx: 'static,
{
    type OutputResource = Acceptor<Listener::Incoming, Ctx, F>;
    type OutputInstaller = FutureInstaller<Self::OutputResource>;
    fn installer(&mut self, _: InputInstaller, name: &str) -> Self::OutputInstaller {
        trace!("Creating future installer for listener {}", name);
        FutureInstaller::default()
    }
    fn transform(
        &mut self,
        mut listener: Listener,
        cfg: &SubFragment,
        name: &'static str,
    ) -> Result<Self::OutputResource, Error> {
        trace!("Creating acceptor for {} on {:?}", name, cfg);
        let ctx = (self.0)(&mut listener, cfg)?;
        let incoming = listener.into_incoming();
        Ok(Acceptor {
            incoming,
            name,
            ctx,
            handler: self.1.clone(),
        })
    }
}

/// A handler newtype to handle each separate connection.
///
/// If this handler is used (wrapping a closure taking the connection and the [`Fragment`] that
/// created the relevant listening socket it comes from), the closure is called for already
/// accepted connection. It shall produce a future and that future is spawned onto a runtime.
///
/// Most of the time this is what you want if you just want to serve some requests over TCP or
/// something similar that has accepting semantics (one socket that listens and produces more
/// sockets).
///
/// For slightly more complex situation, you might want to also consider the
/// [`HandleListenerInit`].
///
/// [`Fragment`]: spirit::Fragment
#[derive(Clone, Debug)]
pub struct HandleListener<F>(pub F);

impl<Listener, InputInstaller, SubFragment, F, Fut>
    Transformation<Listener, InputInstaller, SubFragment> for HandleListener<F>
where
    Listener: IntoIncoming,
    F: Fn(Listener::Connection, &SubFragment) -> Fut + Clone + 'static,
    Fut: IntoFuture<Item = ()>,
    Fut::Error: Into<Error>,
    SubFragment: Clone + Debug + 'static,
{
    type OutputResource = Acceptor<Listener::Incoming, SubFragment, ConfigAdaptor<F>>;
    type OutputInstaller = FutureInstaller<Self::OutputResource>;
    fn installer(&mut self, _: InputInstaller, name: &str) -> Self::OutputInstaller {
        trace!("Creating future installer for listener {}", name);
        FutureInstaller::default()
    }
    fn transform(
        &mut self,
        listener: Listener,
        cfg: &SubFragment,
        name: &'static str,
    ) -> Result<Self::OutputResource, Error> {
        trace!("Creating acceptor for {} on {:?}", name, cfg);
        let cfg = cfg.clone();
        let incoming = listener.into_incoming();
        Ok(Acceptor {
            incoming,
            name,
            ctx: cfg,
            handler: ConfigAdaptor(self.0.clone()),
        })
    }
}
