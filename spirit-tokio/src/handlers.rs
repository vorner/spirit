//! Various [`Transformation`]s for working with resources in async contexts.
//!
//! Oftentimes, one needs to transform some resource into a future and spawn it into the runtime.
//! While possible to do manually, the types here might be a bit more comfortable.

use std::future::Future;
#[cfg(feature = "net")]
use std::pin::Pin;
#[cfg(feature = "net")]
use std::task::{Context, Poll};

use err_context::AnyError;

#[cfg(feature = "net")]
use super::net::Accept;
use super::FutureInstaller;
#[cfg(feature = "net")]
use log::error;
use log::trace;
use pin_project::pin_project;
use spirit::fragment::Transformation;

/// A [`Transformation`] to take a resource, turn it into a future and install it.
pub struct ToFuture<F>(pub F);

impl<F, Fut, R, II, SF> Transformation<R, II, SF> for ToFuture<F>
where
    F: FnMut(R, &SF) -> Result<Fut, AnyError>,
    Fut: Future<Output = ()> + 'static,
{
    type OutputResource = Fut;
    type OutputInstaller = FutureInstaller;
    fn installer(&mut self, _: II, _: &str) -> FutureInstaller {
        FutureInstaller
    }
    fn transform(&mut self, r: R, cfg: &SF, name: &str) -> Result<Fut, AnyError> {
        trace!("Wrapping {} into a future", name);
        (self.0)(r, cfg)
    }
}

/// A [`Transformation`] to take a resource, turn it into a future and install it.
///
/// Unlike [`ToFuture`], this one doesn't pass the configuration to the closure.
///
/// # Examples
///
/// This is mostly the same example as the one at the crate root, but done slightly differently.
/// The future is created later on, during the transformation phase. While a little bit more
/// verbose here, this comes with two advantages:
///
/// * Works with already provided fragments, like the network primitives in [`net`][crate::net]. In
///   that case you might want to prefer the [`ToFuture`], as it also gives access to the original
///   configuration fragment, including any extra configuration for the future.
/// * The future can be an arbitrary anonymous/unnameable type (eg. `impl Future` or an `async`
///   function), there's no need for boxing. This might have slight positive effect on performance.
///
/// ```rust
/// use std::time::Duration;
///
/// use err_context::AnyError;
/// use serde::{Deserialize, Serialize};
/// use spirit::{Empty, Pipeline, Spirit};
/// use spirit::prelude::*;
/// use spirit::fragment::driver::CacheEq;
/// use spirit_tokio::handlers::ToFutureUnconfigured;
/// use structdoc::StructDoc;
///
/// #[derive(Clone, Debug, Deserialize, PartialEq, Serialize, StructDoc)]
/// #[serde(default)]
/// struct MsgCfg {
///     /// A message to print now and then.
///     msg: String,
///     /// Time between printing the message.
///     interval: Duration,
/// }
///
/// impl MsgCfg {
///     async fn run(self) {
///         loop {
///             println!("{}", self.msg);
///             tokio::time::sleep(self.interval).await;
///         }
///     }
/// }
///
/// impl Default for MsgCfg {
///     fn default() -> Self {
///         MsgCfg {
///             msg: "Hello".to_owned(),
///             interval: Duration::from_secs(1),
///         }
///     }
/// }
///
/// spirit::simple_fragment! {
///     impl Fragment for MsgCfg {
///         type Driver = CacheEq<MsgCfg>;
///         // We simply send the configuration forward
///         type Resource = Self;
///         type Installer = ();
///         fn create(&self, _: &'static str) -> Result<Self::Resource, AnyError> {
///             Ok(self.clone())
///         }
///     }
/// }
///
/// /// An application.
/// #[derive(Default, Deserialize, Serialize, StructDoc)]
/// struct AppConfig {
///     #[serde(flatten)]
///     msg: MsgCfg,
/// }
///
/// impl AppConfig {
///     fn msg(&self) -> &MsgCfg {
///         &self.msg
///     }
/// }
///
/// fn main() {
///     Spirit::<Empty, AppConfig>::new()
///         // Will install and possibly cancel and replace the future if the config changes.
///         .with(
///             Pipeline::new("Msg")
///                 .extract_cfg(AppConfig::msg)
///                 // This thing turns it into the future and sets how to install it.
///                 .transform(ToFutureUnconfigured(MsgCfg::run))
///         )
///         // Just an empty body here.
///         .run(|spirit| {
///             // Usually, one would terminate by CTRL+C, but we terminate from here to make sure
///             // the example finishes.
///             spirit.terminate();
///             Ok(())
///         })
/// }
/// ```
pub struct ToFutureUnconfigured<F>(pub F);

impl<F, Fut, R, II, SF> Transformation<R, II, SF> for ToFutureUnconfigured<F>
where
    F: FnMut(R) -> Fut,
    Fut: Future<Output = ()> + 'static,
{
    type OutputResource = Fut;
    type OutputInstaller = FutureInstaller;
    fn installer(&mut self, _: II, _: &str) -> FutureInstaller {
        FutureInstaller
    }
    fn transform(&mut self, r: R, _: &SF, name: &str) -> Result<Fut, AnyError> {
        trace!("Wrapping {} into a future", name);
        Ok((self.0)(r))
    }
}

/// A plumbing type for [`PerConnection`].
#[cfg(feature = "net")]
#[pin_project]
pub struct Acceptor<A, F, C> {
    #[pin]
    accept: A,
    f: F,
    cfg: C,
    name: &'static str,
}

#[cfg(feature = "net")]
impl<A, F, C, Fut> Future for Acceptor<A, F, C>
where
    A: Accept,
    F: FnMut(A::Connection, &C) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    type Output = ();
    fn poll(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<()> {
        let mut me = self.project();
        loop {
            match me.accept.as_mut().poll_accept(ctx) {
                Poll::Ready(Err(e)) => {
                    error!("Giving up acceptor {}: {}", me.name, e);
                    return Poll::Ready(());
                }
                Poll::Ready(Ok(conn)) => {
                    trace!("Got a new connection on {}", me.name);
                    // Poking the borrow checker around the un-pinning, otherwise it is unhappy
                    let fut = (me.f)(conn, me.cfg);
                    tokio::spawn(fut);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// A [`Transformation`] that creates a new future from each accepted connection.
///
/// For each connection yielded from the acceptor (passed in as the input resource), the function
/// is used to create a new future. That new future is spawned into [`tokio`] runtime. Note that
/// even when this resource gets uninstalled, the spawned futures from the connections are left
/// running.
///
/// In case the acceptor yields an error, it is dropped and not used any more. Note that some
/// primitives, like [`TcpListenWithLimits`][crate::net::TcpListenWithLimits] handles errors
/// internally and never returns any, therefore it might be a good candidate for long-running
/// servers.
///
/// # Examples
#[cfg(feature = "net")]
pub struct PerConnection<F>(pub F);

#[cfg(feature = "net")]
impl<F, Fut, A, II, SF> Transformation<A, II, SF> for PerConnection<F>
where
    A: Accept,
    F: Clone + FnMut(A::Connection, &SF) -> Fut + 'static,
    Fut: Future<Output = ()> + 'static,
    SF: Clone + 'static,
{
    type OutputResource = Acceptor<A, F, SF>;
    type OutputInstaller = FutureInstaller;
    fn installer(&mut self, _: II, _: &str) -> FutureInstaller {
        FutureInstaller
    }
    fn transform(
        &mut self,
        accept: A,
        cfg: &SF,
        name: &'static str,
    ) -> Result<Acceptor<A, F, SF>, AnyError> {
        trace!("Creating new acceptor for {}", name);
        let f = self.0.clone();
        let cfg = cfg.clone();
        Ok(Acceptor {
            accept,
            f,
            cfg,
            name,
        })
    }
}

/// A more flexible (and mind-bending) version of [`PerConnection`].
///
/// The [`PerConnection`] applies a closure to each accepted connection. To share the closure
/// between all listeners, the closure is cloned.
///
/// This version's closure is higher-level closure. It is called once for each listener to produce
/// a per-listener closure to handle its connections. In effect, it allows for custom „cloning“ of
/// the closure.
#[cfg(feature = "net")]
pub struct PerConnectionInit<F>(pub F);

#[cfg(feature = "net")]
impl<FA, FC, Fut, A, II, SF> Transformation<A, II, SF> for PerConnectionInit<FA>
where
    A: Accept,
    FA: FnMut(&A, &SF) -> FC + 'static,
    FC: FnMut(A::Connection, &SF) -> Fut + 'static,
    Fut: Future<Output = ()> + 'static,
    SF: Clone + 'static,
{
    type OutputResource = Acceptor<A, FC, SF>;
    type OutputInstaller = FutureInstaller;
    fn installer(&mut self, _: II, _: &str) -> FutureInstaller {
        FutureInstaller
    }
    fn transform(
        &mut self,
        accept: A,
        cfg: &SF,
        name: &'static str,
    ) -> Result<Acceptor<A, FC, SF>, AnyError> {
        trace!("Creating new acceptor for {}", name);
        let f = (self.0)(&accept, cfg);
        let cfg = cfg.clone();
        Ok(Acceptor {
            accept,
            f,
            cfg,
            name,
        })
    }
}
