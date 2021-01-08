//! Handling limits and errors on listening sockets.
//!
//! „Naked“ listening sockets have two important problems:
//!
//! * They sometimes return errors when accepting, which often terminates the stream. Most of these
//!   are actually recoverable error in practice, so the termination seldom makes sense.
//! * They have no limit on how many active connections they have spawned, allowing the application
//!   to grow without limits and eat all OS resources.
//!
//! This module provides tools to address these problems in the form of [`WithListenLimits`]
//! wrapper. There are also type aliases for already wrapped sockets, like [`TcpListenWithLimits`]
//!
//! [`WithListenLimits`]: crate::net::limits::WithListenLimits
//! [`TcpListenWithLimits`]: crate::net::TcpListenWithLimits

use std::cmp;
use std::fmt::Debug;
use std::future::Future;
use std::io::{Error as IoError, SeekFrom};
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use err_context::AnyError;
use log::trace;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use spirit::extension::Extensible;
use spirit::fragment::driver::{CacheSimilar, Comparable, Comparison};
use spirit::fragment::{Fragment, Stackable};
#[cfg(feature = "cfg-help")]
use structdoc::StructDoc;
use structopt::StructOpt;
use tokio::io::{AsyncBufRead, AsyncRead, AsyncSeek, AsyncWrite, ReadBuf};
use tokio::sync::{AcquireError, OwnedSemaphorePermit, Semaphore};
use tokio::time;
#[cfg(feature = "stream")]
use tokio_stream::Stream;

use super::Accept;

/// Additional configuration for limiting of connections & error handling when accepting.
///
/// The canonical implementation is the [`Limits`] ‒ have a look at that.
///
/// If you don't like how/where it gets the configuration, you can provide your own implementation
/// of this trait.
pub trait ListenLimits {
    /// How long to sleep when error happens.
    fn error_sleep(&self) -> Duration;

    /// Maximum number of active connections one instance will have.
    ///
    /// If you don't want the limit, return some huge number (`usize::max_value() / 8 - 1` is
    /// recommended maximum).
    fn max_conn(&self) -> usize;
}

/// A wrapper around a listening socket [`Fragment`] that adds limits and error handling to it.
///
/// There's also the convenience type alias [`WithLimits`].
///
/// Note that the applied limits are per-instance. If there are two sockets in eg
/// `Vec<TcpListenWithLimits>`, their limits are independent. In addition, if a configuration of a
/// socket changes, the old listening socket is destroyed but the old connections are kept around
/// until they terminate. The new listening socket starts with fresh limits, not counting the old
/// connections.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "structdoc", derive(StructDoc))]
#[non_exhaustive]
pub struct WithListenLimits<A, L> {
    /// The inner listener socket.
    ///
    /// This is available publicly to allow reading the extra configuration out of it.
    #[serde(flatten)]
    pub listen: A,

    /// Limits applied to the listener.
    #[serde(flatten)]
    pub limits: L,
}

/// A convenience type alias for the default [`WithListenLimits`] case.
pub type WithLimits<A> = WithListenLimits<A, Limits>;

impl<A, L> Stackable for WithListenLimits<A, L> where A: Stackable {}

impl<A, L> Comparable for WithListenLimits<A, L>
where
    A: Comparable,
    L: PartialEq,
{
    fn compare(&self, other: &Self) -> Comparison {
        let listener_cmp = self.listen.compare(&other.listen);
        if listener_cmp == Comparison::Same && self.limits != other.limits {
            Comparison::Similar
        } else {
            listener_cmp
        }
    }
}

impl<A, L> Fragment for WithListenLimits<A, L>
where
    A: Clone + Debug + Fragment + Comparable,
    L: Clone + Debug + ListenLimits + PartialEq,
{
    type Driver = CacheSimilar<Self>;
    type Installer = ();
    type Seed = A::Seed;
    type Resource = Limited<A::Resource>;
    const RUN_BEFORE_CONFIG: bool = A::RUN_BEFORE_CONFIG;
    fn make_seed(&self, name: &'static str) -> Result<Self::Seed, AnyError> {
        self.listen.make_seed(name)
    }
    fn make_resource(
        &self,
        seed: &mut Self::Seed,
        name: &'static str,
    ) -> Result<Self::Resource, AnyError> {
        let inner = self.listen.make_resource(seed, name)?;
        // :-( Semaphore can't handle more than that
        let limit = cmp::min(self.limits.max_conn(), usize::MAX >> 4);
        Ok(Limited {
            inner,
            error_sleep: self.limits.error_sleep(),
            err_delay: None,
            allowed_conns: Arc::new(Semaphore::new(limit)),
            permit_fut: None,
        })
    }
    fn init<B: Extensible<Ok = B>>(builder: B, name: &'static str) -> Result<B, AnyError>
    where
        B::Config: DeserializeOwned + Send + Sync + 'static,
        B::Opts: StructOpt + Send + Sync + 'static,
    {
        A::init(builder, name)
    }
}

fn default_error_sleep() -> Duration {
    Duration::from_millis(100)
}

/// An implementation of [`ListenLimits`] that reads the limits from configuration.
///
/// # Fields
///
/// * `error-sleep`: The back-off time when non-fatal error happens, in human readable form.
///   Defaults to `100ms` if not present.
/// * `max-conn`: Maximum number of parallel connections on this listener. Defaults to no limit
///   (well, to `usize::max_value() / 8 - 1`, actually, for technical reasons, but that should be
///   effectively no limit).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[non_exhaustive]
pub struct Limits {
    /// How long to wait before trying again after an error.
    ///
    /// Some errors when accepting are simply ignored (eg. the connection was closed by the other
    /// side before we had time to accept it). Some others (eg. too many open files) put the
    /// acceptor into a sleep before it tries again, in the hope the situation will have improved by
    /// then.
    ///
    /// Defaults to `100ms` if not set.
    #[serde(
        rename = "error-sleep",
        default = "default_error_sleep",
        deserialize_with = "spirit::utils::deserialize_duration",
        serialize_with = "spirit::utils::serialize_duration"
    )]
    pub error_sleep: Duration,

    /// Maximum number of connections per one listener.
    ///
    /// If it is reached, more connections will not be accepted until some of the old ones are
    /// terminated.
    ///
    /// Defaults to implementation limits if not set (2^31 - 1 on 32bit systems, 2^63 - 1 on 64bit
    /// systems), which is likely higher than what the OS can effectively handle ‒ so you can
    /// assume that if not set, there's no limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_conn: Option<usize>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            error_sleep: default_error_sleep(),
            max_conn: None,
        }
    }
}

impl ListenLimits for Limits {
    fn error_sleep(&self) -> Duration {
        self.error_sleep
    }
    fn max_conn(&self) -> usize {
        self.max_conn.unwrap_or_else(|| usize::max_value() / 2 - 1)
    }
}

/// Wrapper around a listener instance.
///
/// This is a plumbing type the user shouldn't need to come into contact with. It implements the
/// [`Accept`] trait, which is the interesting property.
///
/// This is created by the [`Fragment`] trait of [`WithListenLimits`].
pub struct Limited<A> {
    inner: A,
    error_sleep: Duration,
    // If there's a global-ish error (too many file descriptors, for example), we want to wait a
    // bit. And here we have the future for the waiting. None if we don't wait.
    err_delay: Option<Pin<Box<dyn Future<Output = ()> + Send + Sync>>>,
    // Tracking how many connections we have/we can still accept.
    allowed_conns: Arc<Semaphore>,
    // Awaiting on a permit. Unfortunately, no way to name the thing like in the case of the delay,
    // so we have allocation and dyn dispatch. But we avoid having this future created on the hot
    // path anyway.
    //
    // We probably could write our own semaphore thing, but that would be hard and the code even
    // more complex, likely. So we mostly hope the limits are not hit very often.
    #[allow(clippy::type_complexity)]
    permit_fut: Option<
        Pin<Box<dyn Future<Output = Result<OwnedSemaphorePermit, AcquireError>> + Send + Sync>>,
    >,
}

impl<A: Accept> Accept for Limited<A> {
    type Connection = Tracked<A::Connection>;
    // :-( This could be much easier with async-await, but we want this to be a trait method with
    // Poll, and named types and Unpin. Which means manual implementation. Doh…
    fn poll_accept(&mut self, ctx: &mut Context) -> Poll<Result<Self::Connection, IoError>> {
        // First, get a permit to create new connections. As the Box<dyn> thing may be more
        // expensive than what we would have liked, we have a fast path first that does not
        // allocate.
        let permit = loop {
            match self.permit_fut.as_mut() {
                // We are waiting for a permit right now, postponed from last time.
                Some(fut) => match fut.as_mut().poll(ctx) {
                    Poll::Ready(permit) => break permit,
                    // Still waiting.
                    Poll::Pending => return Poll::Pending,
                },
                None => {
                    match Arc::clone(&self.allowed_conns).try_acquire_owned() {
                        // The expected fast path… no future allocated on heap, no dyn and we
                        // expect we are in limits most of the time.
                        Ok(permit) => break Ok(permit),
                        Err(_) => {
                            // We have run out of permits for new connections and will probably
                            // have to wait for them. Therefore, we get the future, store it and
                            // let the next iteration retry. That could succeed (in case someone
                            // returns a permit in the meantime) or will at least register the
                            // wakeup for later on.
                            let permit_fut = Arc::clone(&self.allowed_conns).acquire_owned();
                            self.permit_fut = Some(Box::pin(permit_fut));
                            // Continue to the next loop iteration to start the future.
                        }
                    }
                }
            }
        };

        // Due to error handling, we may need to retry (either right away or with delay, but then,
        // we want to poll the delay at least, to register the wakeup).
        loop {
            // Do we have a sleep in process, due to previous errols?
            if let Some(delay) = self.err_delay.as_mut() {
                if Pin::new(delay).poll(ctx).is_ready() {
                    self.err_delay.take(); // Sleeping is over, get to do some work
                } else {
                    return Poll::Pending; // Still sleeping/recovering from an error
                }
            }

            // Is the error local to this particular connection attempt? If so, we can keep trying.
            // If not, we should wait for a bit because things like exhausted file descriptors
            // would be repeating.
            fn is_connection_local(e: &IoError) -> bool {
                use std::io::ErrorKind::*;
                matches!(
                    e.kind(),
                    ConnectionAborted | ConnectionRefused | ConnectionReset
                )
            }

            // We have a permit for another connection, try to get one. If it's not available, we'll
            // simply drop the permit and it'll be available for later.
            match self.inner.poll_accept(ctx) {
                Poll::Ready(Err(ref e)) if is_connection_local(e) => {
                    trace!("Connection attempt error: {}", e);
                    continue;
                }
                Poll::Ready(Err(_)) => {
                    trace!("Accept error, sleeping for {:?}", self.error_sleep);
                    self.err_delay = Some(Box::pin(time::sleep(self.error_sleep)));
                }
                Poll::Ready(Ok(conn)) => {
                    trace!("Got a new connection");
                    return Poll::Ready(Ok(Tracked {
                        inner: conn,
                        _permit: permit.expect("We don't remove the semaphore"),
                    }));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(feature = "stream")]
impl<A> Stream for Limited<A>
where
    A: Accept + Unpin,
{
    type Item = Result<<Self as Accept>::Connection, IoError>;
    fn poll_next(mut self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Option<Self::Item>> {
        self.poll_accept(ctx).map(Some)
    }
}

/// One connection accepted through the [`Limited`].
///
/// Apart from acting (end dereferencing) to the wrapped connection, this also tracks that it's
/// still alive and makes room for more new connections when dropped.
pub struct Tracked<C> {
    inner: C,
    // Just an RAII object to return the permit on drop
    _permit: OwnedSemaphorePermit,
}

impl<C> Deref for Tracked<C> {
    type Target = C;
    fn deref(&self) -> &C {
        &self.inner
    }
}

impl<C> DerefMut for Tracked<C> {
    fn deref_mut(&mut self) -> &mut C {
        &mut self.inner
    }
}

// FIXME: Use pin-project to get rid of the + Unpin requirement? Most our types will, however,
// fulfill it, so we probably don't really care.
impl<C: AsyncBufRead + Unpin> AsyncBufRead for Tracked<C> {
    fn poll_fill_buf(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<&[u8], IoError>> {
        Pin::new(&mut self.get_mut().inner).poll_fill_buf(ctx)
    }
    fn consume(mut self: Pin<&mut Self>, amt: usize) {
        Pin::new(&mut self.inner).consume(amt)
    }
}

impl<C: AsyncRead + Unpin> AsyncRead for Tracked<C> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        ctx: &mut Context,
        buf: &mut ReadBuf,
    ) -> Poll<Result<(), IoError>> {
        Pin::new(&mut self.inner).poll_read(ctx, buf)
    }
}

impl<C: AsyncSeek + Unpin> AsyncSeek for Tracked<C> {
    fn start_seek(mut self: Pin<&mut Self>, position: SeekFrom) -> Result<(), IoError> {
        Pin::new(&mut self.inner).start_seek(position)
    }
    fn poll_complete(mut self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Result<u64, IoError>> {
        Pin::new(&mut self.inner).poll_complete(ctx)
    }
}

impl<C: AsyncWrite + Unpin> AsyncWrite for Tracked<C> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        ctx: &mut Context,
        buf: &[u8],
    ) -> Poll<Result<usize, IoError>> {
        Pin::new(&mut self.inner).poll_write(ctx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Result<(), IoError>> {
        Pin::new(&mut self.inner).poll_flush(ctx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Result<(), IoError>> {
        Pin::new(&mut self.inner).poll_shutdown(ctx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        ctx: &mut Context,
        bufs: &[std::io::IoSlice],
    ) -> Poll<Result<usize, IoError>> {
        Pin::new(&mut self.inner).poll_write_vectored(ctx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use spirit::Empty;
    use tokio::net::TcpStream;
    use tokio::sync::oneshot::{self, Receiver};
    use tokio::time;

    use super::*;
    use crate::net::{Listen, TcpListen};

    fn listener() -> (Limited<impl Accept>, SocketAddr) {
        let incoming_cfg = WithListenLimits {
            listen: TcpListen {
                listen: Listen {
                    host: IpAddr::V4(Ipv4Addr::LOCALHOST),
                    ..Listen::default()
                },
                tcp_config: Empty {},
                extra_cfg: Empty {},
            },
            limits: Limits {
                error_sleep: Duration::from_millis(100),
                max_conn: Some(2),
            },
        };

        let mut seed = incoming_cfg.make_seed("test_listener").unwrap();
        let addr = seed.local_addr().unwrap();
        let listener = incoming_cfg
            .make_resource(&mut seed, "test_listener")
            .unwrap();

        assert_eq!(2, listener.allowed_conns.available_permits());

        (listener, addr)
    }

    /// Make 3 connections to the given address.
    async fn connector(addr: SocketAddr, done: Receiver<()>) {
        let _conn1 = TcpStream::connect(addr).await.unwrap();
        let _conn2 = TcpStream::connect(addr).await.unwrap();
        // The last one may or may not take some time, probably depends on some kind of
        // in-kernel buffers. For that we just wait for confirmation from the acceptor.
        let _conn3 = TcpStream::connect(addr).await.unwrap();
        done.await.unwrap();
    }

    /// Check that setting the limits has some effect.
    ///
    /// Try to accept more connections than allowed. After dropping one, try to acquire the extra
    /// again.
    #[tokio::test]
    async fn conn_limit() {
        let (mut listener, addr) = listener();

        let (done_send, done_recv) = oneshot::channel();

        let connector = tokio::spawn(connector(addr, done_recv));

        let acceptor = tokio::spawn(async move {
            let conn1 = listener.accept().await.unwrap();
            let _conn2 = listener.accept().await.unwrap();

            // No more free permits
            assert_eq!(0, listener.allowed_conns.available_permits());

            let over_limit = listener.accept(); // No await, only prepare the future
            let over_limit = time::timeout(Duration::from_millis(50), over_limit);
            assert!(over_limit.await.is_err(), "Accepted extra connection");

            // Make room for new connection
            drop(conn1);

            let _conn3 = listener.accept().await.unwrap();
            done_send.send(()).unwrap();
        });

        time::timeout(Duration::from_secs(5), async {
            acceptor.await.unwrap();
            connector.await.unwrap();
        })
        .await
        .expect("Didn't finish test in time");
    }

    /// Similar to above, but we don't check for the fact we don't get a connection.
    ///
    /// Instead we make sure the future eventually does return something. We do that by timing out
    /// the connections in background.
    #[tokio::test]
    async fn conn_limit_cont() {
        let (mut listener, addr) = listener();

        let (done_send, done_recv) = oneshot::channel();

        let connector = tokio::spawn(connector(addr, done_recv));

        let acceptor = tokio::spawn(async move {
            for _ in 0..3 {
                let conn = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    time::sleep(Duration::from_millis(50)).await;
                    drop(conn);
                });
            }

            done_send.send(()).unwrap();
        });

        time::timeout(Duration::from_secs(5), async {
            acceptor.await.unwrap();
            connector.await.unwrap();
        })
        .await
        .expect("Didn't finish test in time");
    }
}
