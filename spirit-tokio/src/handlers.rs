use std::fmt::Debug;
use std::io::Error as IoError;
use std::sync::Arc;

use failure::{Error, Fail};
use futures::{Async, Future, IntoFuture, Poll, Stream};
use spirit::fragment::Transformation;

use installer::FutureInstaller;
use net::IntoIncoming;

#[derive(Clone, Debug)]
struct HandleSocket<F>(F);

impl<Socket, InputInstaller, SubFragment, F, R> Transformation<Socket, InputInstaller, SubFragment>
    for HandleSocket<F>
where
    F: FnMut(Socket, &SubFragment) -> Result<R, Error>,
    R: 'static,
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

struct Acceptor<Incoming, Ctx, F> {
    name: &'static str,
    incoming: Incoming,
    ctx: Ctx,
    f: F,
}

impl<Incoming, Ctx, F, Fut> Future for Acceptor<Incoming, Ctx, F>
where
    Incoming: Stream<Error = IoError>,
    F: Fn(Incoming::Item, &mut Ctx) -> Fut,
    Fut: IntoFuture<Item = ()>,
    Fut::Future: Send + 'static,
    Fut::Error: Into<Error>,
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
                let future = (self.f)(conn, &mut self.ctx)
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
struct HandleListenerInit<I, F>(I, F);

impl<Listener, InputInstaller, SubFragment, I, Ctx, F, Fut>
    Transformation<Listener, InputInstaller, SubFragment> for HandleListenerInit<I, F>
where
    Listener: IntoIncoming,
    I: FnMut(&mut Listener, &SubFragment) -> Result<Ctx, Error>,
    F: Fn(Listener::Connection, &mut Ctx) -> Fut + Clone + 'static,
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
            f: self.1.clone(),
        })
    }
}

pub fn handle_socket<InputInstaller, Socket, SubFragment, F, Fut>(
    f: F,
) -> impl Transformation<Socket, InputInstaller, SubFragment>
where
    F: FnMut(Socket, &SubFragment) -> Result<Fut, Error>,
    Fut: 'static,
    SubFragment: Debug,
{
    HandleSocket(f)
}

pub fn handle_socket_simple<InputInstaller, Socket, SubFragment, F, Fut>(
    mut f: F,
) -> impl Transformation<Socket, InputInstaller, SubFragment>
where
    F: FnMut(Socket) -> Result<Fut, Error>,
    Fut: 'static,
    SubFragment: Debug,
{
    let wrapper = move |sock: Socket, _cfg: &SubFragment| f(sock);
    handle_socket(wrapper)
}

pub fn handle_listener_init<InputInstaller, Listener, SubFragment, I, Ctx, F, Fut>(
    init: I,
    f: F,
) -> impl Transformation<Listener, InputInstaller, SubFragment>
where
    Listener: IntoIncoming,
    I: FnMut(&mut Listener, &SubFragment) -> Result<Ctx, Error>,
    F: Fn(Listener::Connection, &mut Ctx) -> Fut + 'static,
    SubFragment: Debug,
    Ctx: 'static,
{
    let f = Arc::new(f);
    let f = move |conn: Listener::Connection, ctx: &mut Ctx| f(conn, ctx);
    HandleListenerInit(init, f)
}

pub fn handle_listener<InputInstaller, Listener, SubFragment, F, Fut>(
    f: F,
) -> impl Transformation<Listener, InputInstaller, SubFragment>
where
    Listener: IntoIncoming,
    F: Fn(Listener::Connection, &SubFragment) -> Fut + 'static,
    SubFragment: Clone + Debug + 'static,
{
    let init =
        |_: &mut Listener, cfg: &SubFragment| -> Result<SubFragment, Error> { Ok(cfg.clone()) };
    let f = move |conn: Listener::Connection, cfg: &mut SubFragment| f(conn, cfg);
    handle_listener_init(init, f)
}

pub fn handle_listener_simple<InputInstaller, Listener, SubFragment, F, Fut>(
    f: F,
) -> impl Transformation<Listener, InputInstaller, SubFragment>
where
    Listener: IntoIncoming,
    F: Fn(Listener::Connection) -> Fut + 'static,
    SubFragment: Debug,
{
    let init = |_: &mut Listener, _: &SubFragment| -> Result<(), Error> { Ok(()) };
    let f = move |conn: Listener::Connection, _: &mut ()| f(conn);
    handle_listener_init(init, f)
}
