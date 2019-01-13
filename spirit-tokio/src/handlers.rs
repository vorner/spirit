use std::fmt::Debug;
use std::io::Error as IoError;

use failure::{Error, Fail};
use futures::{Async, Future, IntoFuture, Poll, Stream};
use spirit::fragment::Transformation;

use installer::FutureInstaller;
use net::IntoIncoming;

#[derive(Clone, Debug)]
pub struct HandleSocket<F>(pub F);

impl<Socket, InputInstaller, SubFragment, F, R> Transformation<Socket, InputInstaller, SubFragment>
    for HandleSocket<F>
where
    F: FnMut(Socket, &SubFragment) -> Result<R, Error>,
    R: 'static,
    SubFragment: Debug,
    R: IntoFuture<Item = (), Error = ()>,
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
    <<Handler as ConnectionHandler<Incoming::Item, Ctx>>::Output as IntoFuture>::Future: Send + 'static,
    <<Handler as ConnectionHandler<Incoming::Item, Ctx>>::Output as IntoFuture>::Error: Into<Error>,
{
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<(), ()> {
        loop {
            let incoming = self.incoming.poll().map_err(|e| {
                let e = e.context("Listening socket terminated unexpectedly").into();
                spirit::utils::log_error(module_path!(), &e);
            });
            let connection = try_ready!(incoming);
            if let Some(conn) = connection {
                let name = self.name;
                let future = self.handler.execute(conn, &mut self.ctx)
                    .into_future()
                    .map_err(move |e| {
                        let e = e
                            .into()
                            .context(format!("Failed to handle connection on {}", name));
                        spirit::utils::log_error(module_path!(), &e.into());
                    });
                tokio::spawn(future);
            } else {
                warn!("The listening socket on {} terminated", self.name);
                return Ok(Async::Ready(()));
            }
        }
    }
}

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
