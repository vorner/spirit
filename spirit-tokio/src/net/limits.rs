//! Handling limits and errors on listening sockets.
//!
//! „Naked“ listening sockets have two important problems:
//!
//! * They sometimes return errors when accepting, which often terminates the stream.
//! * They have no limit on how many active connections they have spawned.
//!
//! This module provides tools to address these problems.

use std::fmt::Debug;
use std::io::{Error as IoError, Read, Write};
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use failure::Error;
use futures::task::AtomicTask;
use futures::{Async, Poll, Stream};
use serde::ser::Serializer;
use spirit::validation::Results as ValidationResults;
use spirit::Builder;
use tk_listen::{ListenExt, SleepOnError};
use tokio::io::{AsyncRead, AsyncWrite};

use super::IntoIncoming;
use base_traits::{ExtraCfgCarrier, Name, ResourceConfig};

/// Additional configuration for limiting of connections & error handling when accepting.
///
/// The canonical implementation is the [`WithListenLimits`] ‒ have a look at that.
///
/// If you don't like how/where it gets the configuration, you can provide your own implementation
/// of this trait. It'll implement the [`ResourceConfig`].
pub trait ListenLimits {
    /// The configuration/[`ResourceConfig`] of the real inner listener to handle errors and limits
    /// on.
    type Listener;

    /// Access to the listener configuration.
    fn listener(&self) -> &Self::Listener;

    /// How long to sleep when error happens.
    fn error_sleep(&self) -> Duration;

    /// Maximum number of active connections one instance will have.
    ///
    /// If you don't want the limit, return some huge number (`usize::max_value() / 2 - 1` is
    /// recommended maximum).
    fn max_conn(&self) -> usize;
}

impl<O, C, Limited> ResourceConfig<O, C> for Limited
where
    Limited: ListenLimits + Debug + ExtraCfgCarrier + PartialEq + Send + Sync + 'static,
    Limited::Listener: ResourceConfig<O, C>,
{
    type Seed = <Limited::Listener as ResourceConfig<O, C>>::Seed;
    type Resource = LimitedListener<<Limited::Listener as ResourceConfig<O, C>>::Resource>;
    fn create(&self, name: &str) -> Result<Self::Seed, Error> {
        self.listener().create(name)
    }
    fn fork(&self, seed: &Self::Seed, name: &str) -> Result<Self::Resource, Error> {
        let inner = self.listener().fork(seed, name)?;
        Ok(LimitedListener {
            inner,
            error_sleep: self.error_sleep(),
            max_conn: self.max_conn(),
        })
    }
    fn scaled(&self, name: &str) -> (usize, ValidationResults) {
        self.listener().scaled(name)
    }
    fn is_similar(&self, other: &Self, name: &str) -> bool {
        self.listener().is_similar(&other.listener(), name)
    }
    fn install<N: Name>(builder: Builder<O, C>, name: &N) -> Builder<O, C> {
        Limited::Listener::install(builder, name)
    }
}

fn default_error_sleep() -> Duration {
    Duration::from_millis(100)
}

fn serialize_duration<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&::humantime::format_duration(*d).to_string())
}

/// Wrapper to enrich inner configuration fragment with handling of listen limits.
///
/// When a resource config for a listening socket is wrapped in this, the returned resource will
/// handle errors internally (some of them will be simply ignored ‒ like if the socket was closed
/// before we even accepted it, some will lead to waiting a short time ‒ like „Too many open
/// files“). It'll also put a limit to number of active accepted connections ‒ if the limit is
/// reached, no more connections are accepted until some are closed.
///
/// You probably want to wrap most of the listening sockets in this.
///
/// If you want non-default loading of configuration, but still want the functionality, look at the
/// [`ListenLimits`] trait.
///
/// # Fields
///
/// In addition to what the inner `Listener` contains, this adds these fields (that directly
/// correspond to the methods on [`ListenLimits`]):
///
/// * `error-sleep`: The back-off time when non-fatal error happens, in human readable form.
///   Defaults to `100ms` if not present.
/// * `max-conn`: Maximum number of parallel connections on this listener. Defaults to no limit
///   (well, to `usize::max_value() / 2 - 1`, actually, for technical reasons, but that should be
///   effectively no limit).
///
/// # Notes
///
/// This is per one instance. In case it scales to multiple instances, each one has a separate
/// connection limit. See [`Scaled`].
///
/// Also, when the resource is reinitialized (for example when the configuration changes), the new
/// instance gets a fresh limit.
///
/// [`per_connection`]: ::per_connection
/// [`Scaled`]: ::scaled::Scaled
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
pub struct WithListenLimits<Listener> {
    #[serde(flatten)]
    inner: Listener,

    /// How long to wait before trying again after an error.
    ///
    /// Some errors when accepting are simply ignored (eg. the connection was closed by the other
    /// side before we had time to accept it). Some others (eg. too many open files) put the
    /// acceptor into a sleep before it tries again, in the hope the situation will improve until
    /// then.
    ///
    /// Defaults to `100ms` if not set.
    #[serde(
        rename = "error-sleep",
        default = "default_error_sleep",
        deserialize_with = "::serde_humantime::deserialize",
        serialize_with = "serialize_duration"
    )]
    error_sleep: Duration,

    /// Maximum number of connections per one listener.
    ///
    /// If it is reached, more connections will not be accepted until some of the old ones are
    /// terminated.
    ///
    /// Default to implementation limits if not set (2^31 - 1 on 32bit systems, 2^63 - 1 on 64bit
    /// systems), which is likely higher than what the OS can effectively handle.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_conn: Option<usize>,
}

impl<Listener: Default> Default for WithListenLimits<Listener> {
    fn default() -> Self {
        Self {
            inner: Listener::default(),
            error_sleep: default_error_sleep(),
            max_conn: None,
        }
    }
}

impl<Listener> ListenLimits for WithListenLimits<Listener> {
    type Listener = Listener;
    fn listener(&self) -> &Listener {
        &self.inner
    }
    fn error_sleep(&self) -> Duration {
        self.error_sleep
    }
    fn max_conn(&self) -> usize {
        self.max_conn.unwrap_or_else(|| usize::max_value() / 2 - 1)
    }
}

delegate_resource_traits! {
    delegate ExtraCfgCarrier to inner on WithListenLimits;
}

cfg_helpers! {
    impl helpers for WithListenLimits<Listener> where;
}

/// Wrapper around a listener instance.
///
/// This is a plumbing type the user shouldn't need to come into contact with. It implements the
/// [`IntoIncoming`] trait, which is the interesting property.
///
/// This is created by the [`ResourceConfig`] trait of [`ListenLimits`].
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct LimitedListener<Inner> {
    inner: Inner,
    error_sleep: Duration,
    max_conn: usize,
}

impl<Inner: IntoIncoming> IntoIncoming for LimitedListener<Inner> {
    type Connection = LimitedConn<Inner::Connection>;
    type Incoming = LimitedIncoming<Inner::Incoming>;
    fn into_incoming(self) -> Self::Incoming {
        let inner = self.inner.into_incoming().sleep_on_error(self.error_sleep);
        LimitedIncoming {
            inner,
            limit: Arc::new(ConnLimit {
                max_conn: self.max_conn,
                active_cnt: AtomicUsize::new(0),
                wakeup: AtomicTask::new(),
            }),
        }
    }
}

struct ConnLimit {
    max_conn: usize,
    // 2 * count of connections + I'm blocked flag
    active_cnt: AtomicUsize,
    wakeup: AtomicTask,
}

// # Encoding of active_cnt
//
// The active_cnt serves dual role. It contains the number of active connections, but multiplied by
// two. The lowest bit is used as a flag that the accept stream is being blocked because there are
// too many connections and should be notified.
//
// We lower the number of allowed active connections by that. However, that is considered OK,
// because no sane machine is able to keep 2^31 (on 32bit machines) or even 2^63 connections open.
// And even if they did, we would just miscount the connections at occasion, making the limit
// ineffective ‒ but with such limit, it is *already* ineffective.
//
// # Orderings in here
//
// Most of the time we use the active_cnt only as a counter of how many connections we have. For
// that, relaxed would be enough.
//
// There's however one „happens-before“ edge we need to have and that's when we return NotReady. In
// such case, we need to make sure by the time someone reads the flag, the wakeup is already set.
// Therefore, we do the setting of the flag with Release ordering. The reading of the value (well,
// decreasing) in the connection destructor uses Acquire.
//
// To make sure the edge is propagated to all the threads, not only to the first one to touch it,
// all operations that modify it are in fact upgraded to AcqRel (the edge goes into them and out
// again, chaining them together).
//
// Using relaxed in case the compare-exchange fails is OK, because we don't set the flag. In such
// case, the number of connections dropped again between we first checked, so it doesn't make sense
// to really block. But for simplicity, we just retry by notifying ourselves (it's simple to reason
// about and should happen only in really rare corner cases, so the waste performance is OK).
impl ConnLimit {
    fn check(&self) -> bool {
        let cnt_orig = self.active_cnt.load(Ordering::Relaxed);
        let registered = cnt_orig % 2 == 1;
        let cnt = cnt_orig / 2;
        if cnt >= self.max_conn {
            self.wakeup.register();
            let cnt_new = cnt * 2 + 1;
            if self
                .active_cnt
                .compare_exchange(cnt_orig, cnt_new, Ordering::AcqRel, Ordering::Relaxed)
                .is_err()
            {
                self.wakeup.notify();
            }
            return false;
        } else if registered {
            self.active_cnt.fetch_sub(1, Ordering::Relaxed);
        }
        true
    }
    fn dec(&self) {
        let prev = self.active_cnt.fetch_sub(2, Ordering::Relaxed);
        if prev % 2 == 1 && prev / 2 >= self.max_conn {
            self.wakeup.notify()
        }
    }
}

/// A wrapper around the incoming stream of connections, providing error handling and limits.
///
/// This is what will come of the [`ResourceConfig`] from [`ListenLimits`]. It is a stream of
/// accepted connections, but without the errors and slowing down when a limit is reached.
pub struct LimitedIncoming<Inner> {
    inner: SleepOnError<Inner>,
    limit: Arc<ConnLimit>,
}

impl<Inner> Stream for LimitedIncoming<Inner>
where
    Inner: Stream<Error = IoError>,
{
    type Item = LimitedConn<Inner::Item>;
    type Error = IoError;
    fn poll(&mut self) -> Poll<Option<Self::Item>, IoError> {
        if !self.limit.check() {
            return Ok(Async::NotReady);
        }
        self.inner
            .poll()
            .map(|a| {
                a.map(|o| {
                    o.map(|i| {
                        self.limit.active_cnt.fetch_add(2, Ordering::AcqRel);
                        LimitedConn {
                            inner: i,
                            limit: Arc::clone(&self.limit),
                        }
                    })
                })
            })
            .map_err(|()| unreachable!("SleepOnError doesn't error, it sleeps"))
    }
}

/// One connection accepted through something configured with [`ListenLimits`].
///
/// It is just a thin wrapper around the real connection, allowing to track how many of them there
/// are.
pub struct LimitedConn<Inner> {
    inner: Inner,
    limit: Arc<ConnLimit>,
}

impl<Inner> Drop for LimitedConn<Inner> {
    fn drop(&mut self) {
        self.limit.dec()
    }
}

impl<I: Read> Read for LimitedConn<I> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        self.inner.read(buf)
    }
}

impl<I: Write> Write for LimitedConn<I> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, IoError> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> Result<(), IoError> {
        self.inner.flush()
    }
}

impl<I: AsyncRead> AsyncRead for LimitedConn<I> {}

impl<I: AsyncWrite> AsyncWrite for LimitedConn<I> {
    fn shutdown(&mut self) -> Poll<(), IoError> {
        self.inner.shutdown()
    }
}

impl<I> Deref for LimitedConn<I> {
    type Target = I;
    fn deref(&self) -> &I {
        &self.inner
    }
}

impl<I> DerefMut for LimitedConn<I> {
    fn deref_mut(&mut self) -> &mut I {
        &mut self.inner
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    // corona is more heavy-weight than bare-bones tokio, but more comfortable and who cares in
    // tests
    use corona::coroutine::CleanupStrategy;
    use corona::prelude::*;
    use spirit::Empty;
    use tokio::clock;
    use tokio::net::TcpStream;
    use tokio::prelude::FutureExt;
    use tokio::timer::Delay;

    use super::*;
    use net::{Listen, TcpListen};

    #[test]
    fn conn_limit() {
        Coroutine::new()
            .stack_size(40_960)
            .cleanup_strategy(CleanupStrategy::LeakOnPanic)
            .run(|| {
                let incoming_cfg = WithListenLimits {
                    inner: TcpListen {
                        listen: Listen {
                            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
                            ..Listen::default()
                        },
                        tcp_config: Empty {},
                        extra_cfg: Empty {},
                        scale: Empty {},
                    },
                    error_sleep: Duration::from_millis(100),
                    max_conn: Some(2),
                };
                let seed =
                    ResourceConfig::<Empty, Empty>::create(&incoming_cfg, "test_listener").unwrap();
                let addr = seed.local_addr().unwrap();
                let mut incoming =
                    ResourceConfig::<Empty, Empty>::fork(&incoming_cfg, &seed, "test_listener")
                        .unwrap()
                        .into_incoming();
                assert_eq!(2, incoming.limit.max_conn);
                assert_eq!(0, incoming.limit.active_cnt.load(Ordering::Relaxed));

                corona::spawn(move || {
                    let conn1 = incoming.coro_next().unwrap().unwrap();
                    let _conn2 = incoming.coro_next().unwrap().unwrap();
                    assert_eq!(4, incoming.limit.active_cnt.load(Ordering::Relaxed));
                    let maybe = incoming
                        .extractor()
                        .timeout(Duration::from_millis(50))
                        .coro_wait();
                    // This one timed out because we don't have more space
                    assert!(maybe.is_err());
                    Delay::new(clock::now() + Duration::from_millis(500))
                        .coro_wait()
                        .unwrap();
                    // But if we drop one, we have place for another one.
                    drop(maybe);
                    drop(conn1);
                    let _conn3 = incoming.coro_next().unwrap().unwrap();
                    assert_eq!(4, incoming.limit.active_cnt.load(Ordering::Relaxed));
                });
                // Two are the limit
                let _conn1 = TcpStream::connect(&addr).coro_wait().unwrap();
                let _conn2 = TcpStream::connect(&addr).coro_wait().unwrap();
                // This might take time, but sometimes doesn't (the kernel buffer?)
                let _extra = TcpStream::connect(&addr).coro_wait().unwrap();
                // Give it time to process the last one too.
                Delay::new(clock::now() + Duration::from_millis(1000))
                    .coro_wait()
                    .unwrap();
            })
            .unwrap();
    }
}
