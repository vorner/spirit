//! Support for alternative choices of configuration.

use std::io::{BufRead, Error as IoError, Read, Seek, SeekFrom, Write};

use failure::Error;
use futures::future::Either as FutEither;
use futures::{Async, Future, Poll, Sink, StartSend, Stream};
use spirit::validation::Results as ValidationResults;
use spirit::Builder;
use tokio::io::{AsyncRead, AsyncWrite};

use base_traits::{ExtraCfgCarrier, Name, ResourceConfig};
use net::IntoIncoming;

/// The [`Either`] type allows to wrap two similar [`ResourceConfig`]s and let the user choose
/// which one will be used.
///
/// For example, if your server could run both on common TCP and unix domain stream sockets, you
/// could use the `Either<TcpListen, UnixListen>`. This fragment would then create resources of
/// type `Either<TcpListener, UnixListener>`.
///
/// Many traits are delegated through to one or the other instance inside (in case both implement
/// it). So, the above resource will implement the [`IntoIncoming`] trait that will accept
/// instances of `Either<TcpStream, UnixStream>`. These'll in turn implement [`AsyncRead`] and
/// [`AsyncWrite`], therefore can be handled uniformly just as connections. Similarly, the original
/// resource will implement [`ExtraCfgCarrier`].
///
/// # Deserialization
///
/// This uses the [untagged] serde attribute. This means there are no additional configuration
/// options present and the choice is made by trying to first deserialize the [`A`] variant and
/// if that fails, trying the [`B`] one. Therefore, the inner resource configs need to have some
/// distinct fields. In our example, this would parse as [`TcpListen`]:
///
/// ```toml
/// [[listen]]
/// port = 1234
/// ```
///
/// While this as an [`UnixListen`]:
///
/// ```toml
/// [[listen]]
/// path = "/tmp/socket"
/// ```
///
/// If you need different parsing, you can use either a newtype (and delegate relevant traits into
/// it with the [`macros`]) or [remote derive].
///
/// # Other similar types
///
/// This is not the only `Either` type around. Unfortunately, none of the available ones was just
/// right for the use case here, so this crate rolls its own. But it provides [`From`]/[`Into`]
/// conversions between them:
///
/// * [`futures::future::Either`]
/// * The [`either`](https://crates.io/crates/either) crate (under the `either` feature flag, off
///   by default).
///
/// # More than two options
///
/// This allows only two variants. However, if you need more, it is possible to nest them and form
/// a tree.
///
/// # Examples
///
/// ```rust
/// extern crate failure;
/// extern crate serde;
/// #[macro_use]
/// extern crate serde_derive;
/// extern crate spirit;
/// extern crate spirit_tokio;
/// extern crate tokio;
///
/// use std::sync::Arc;
///
/// use failure::Error;
/// use spirit::{Empty, Spirit};
/// #[cfg(unix)]
/// use spirit_tokio::either::Either;
/// use spirit_tokio::net::{TcpListen, IntoIncoming};
/// use spirit_tokio::net::limits::WithListenLimits;
/// #[cfg(unix)]
/// use spirit_tokio::net::unix::UnixListen;
/// use spirit_tokio::ResourceConfig;
/// use tokio::prelude::*;
///
/// // If we want to work on systems that don't have unix domain sockets...
///
/// #[cfg(unix)]
/// type Listener = WithListenLimits<Either<TcpListen, UnixListen>>;
/// #[cfg(not(unix))]
/// type Listener = WithListenLimits<TcpListen>;
///
/// type Connection =
///     <<Listener as ResourceConfig<Empty, Config>>::Resource as IntoIncoming>::Connection;
///
/// const DEFAULT_CONFIG: &str = r#"
/// [[listening_socket]]
/// port = 1235
/// max-conn = 20
/// error-sleep = "100ms"
/// "#;
/// #[derive(Default, Deserialize)]
/// struct Config {
///     listening_socket: Vec<Listener>,
/// }
///
/// impl Config {
///     fn listening_socket(&self) -> Vec<Listener> {
///         self.listening_socket.clone()
///     }
/// }
///
/// fn connection(
///     _: &Arc<Spirit<Empty, Config>>,
///     _: &Arc<Listener>,
///     conn: Connection,
///     _: &str
/// ) -> impl Future<Item = (), Error = Error> {
///     tokio::io::write_all(conn, "Hello\n")
///         .map(|_| ())
///         .map_err(Error::from)
/// }
///
/// fn main() {
///     Spirit::<Empty, Config>::new()
///         .config_defaults(DEFAULT_CONFIG)
///         .config_helper(
///             Config::listening_socket,
///             spirit_tokio::per_connection(connection),
///             "Listener",
///         )
///         .run(|spirit| {
/// #           let spirit = Arc::clone(spirit);
/// #           std::thread::spawn(move || spirit.terminate());
///             Ok(())
///         });
/// }
/// ```
///
/// [untagged]: https://serde.rs/container-attrs.html#untagged
/// [remote derive]: https://serde.rs/remote-derive.html
/// [`TcpListen`]: ::TcpListen
/// [`UnixListen`]: ::net::unix::UnixListen
/// [`macros`]: ::macros
#[derive(Copy, Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[serde(untagged)]
pub enum Either<A, B> {
    #[allow(missing_docs)]
    A(A),
    #[allow(missing_docs)]
    B(B),
}

use self::Either::{A, B};

impl<T> Either<T, T> {
    /// Extracts the inner value in case both have the same type.
    ///
    /// Sometimes, a series of operations produces an `Either` with both types the same. In such
    /// case, `Either` plays no role anymore and this method can be used to get to the inner value.
    pub fn into_inner(self) -> T {
        match self {
            A(a) => a,
            B(b) => b,
        }
    }
}

impl<A, B> From<FutEither<A, B>> for Either<A, B> {
    fn from(e: FutEither<A, B>) -> Self {
        match e {
            FutEither::A(a) => A(a),
            FutEither::B(b) => B(b),
        }
    }
}

impl<A, B> Into<FutEither<A, B>> for Either<A, B> {
    fn into(self) -> FutEither<A, B> {
        match self {
            A(a) => FutEither::A(a),
            B(b) => FutEither::B(b),
        }
    }
}

#[cfg(feature = "either")]
mod other_either {
    extern crate either;

    use self::either::Either as OtherEither;
    use super::*;

    impl<A, B> From<OtherEither<A, B>> for Either<A, B> {
        fn from(e: OtherEither<A, B>) -> Self {
            match e {
                OtherEither::Left(a) => A(a),
                OtherEither::Right(b) => B(b),
            }
        }
    }

    impl<A, B> Into<OtherEither<A, B>> for Either<A, B> {
        fn into(self) -> OtherEither<A, B> {
            match self {
                A(a) => OtherEither::Left(a),
                B(b) => OtherEither::Right(b),
            }
        }
    }

}

macro_rules! either {
    ($either: expr, $pat: pat => $res: expr) => {
        match $either {
            A($pat) => A($res),
            B($pat) => B($res),
        }
    };
}

impl<A, B> ExtraCfgCarrier for Either<A, B>
where
    A: ExtraCfgCarrier,
    // For the user/extra config, it makes sense for them to be the same
    B: ExtraCfgCarrier<Extra = A::Extra>,
{
    type Extra = A::Extra;
    fn extra(&self) -> &Self::Extra {
        either!(self, v => v.extra()).into_inner()
    }
}

impl<A, B, O, C> ResourceConfig<O, C> for Either<A, B>
where
    A: ResourceConfig<O, C>,
    B: ResourceConfig<O, C> + ExtraCfgCarrier<Extra = A::Extra>,
{
    type Seed = Either<A::Seed, B::Seed>;
    type Resource = Either<A::Resource, B::Resource>;
    fn create(&self, name: &str) -> Result<Self::Seed, Error> {
        Ok(either!(self, v => v.create(name)?))
    }
    fn fork(&self, seed: &Self::Seed, name: &str) -> Result<Self::Resource, Error> {
        let res = match (self, seed) {
            (A(me), A(seed)) => A(me.fork(seed, name)?),
            (B(me), B(seed)) => B(me.fork(seed, name)?),
            _ => unreachable!("Someone mixed the seeds"),
        };
        Ok(res)
    }
    fn scaled(&self, name: &str) -> (usize, ValidationResults) {
        either!(self, v => v.scaled(name)).into_inner()
    }
    fn is_similar(&self, other: &Self, name: &str) -> bool {
        match (self, other) {
            (A(me), A(other)) => me.is_similar(other, name),
            (B(me), B(other)) => me.is_similar(other, name),
            // Completely different kind of thing, not similar at all
            _ => false,
        }
    }
    fn install<N: Name>(builder: Builder<O, C>, name: &N) -> Builder<O, C> {
        let builder = A::install(builder, name);
        B::install(builder, name)
    }
}

impl<A, B> IntoIncoming for Either<A, B>
where
    A: IntoIncoming,
    B: IntoIncoming,
{
    type Connection = Either<A::Connection, B::Connection>;
    type Incoming = Either<A::Incoming, B::Incoming>;
    fn into_incoming(self) -> Self::Incoming {
        either!(self, v => v.into_incoming())
    }
}

impl<A, B> Future for Either<A, B>
where
    A: Future,
    B: Future<Error = A::Error>,
{
    type Item = Either<A::Item, B::Item>;
    type Error = A::Error;
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        match self {
            A(a) => a.poll().map(|a| a.map(A)),
            B(b) => b.poll().map(|a| a.map(B)),
        }
    }
}

impl<A, B> Sink for Either<A, B>
where
    A: Sink,
    B: Sink<SinkError = A::SinkError, SinkItem = A::SinkItem>,
{
    type SinkItem = A::SinkItem;
    type SinkError = A::SinkError;
    fn start_send(&mut self, item: Self::SinkItem) -> StartSend<Self::SinkItem, Self::SinkError> {
        either!(self, v => v.start_send(item)).into_inner()
    }
    fn poll_complete(&mut self) -> Poll<(), Self::SinkError> {
        either!(self, v => v.poll_complete()).into_inner()
    }
}

impl<A, B> Stream for Either<A, B>
where
    A: Stream,
    B: Stream<Error = A::Error>,
{
    type Item = Either<A::Item, B::Item>;
    type Error = A::Error;
    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        match self {
            A(a) => a.poll().map(|a| a.map(|o| o.map(A))),
            B(b) => b.poll().map(|a| a.map(|o| o.map(B))),
        }
    }
}

impl<A, B> Read for Either<A, B>
where
    A: Read,
    B: Read,
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        either!(self, v => v.read(buf)).into_inner()
    }
}

impl<A, B> BufRead for Either<A, B>
where
    A: BufRead,
    B: BufRead,
{
    fn fill_buf(&mut self) -> Result<&[u8], IoError> {
        either!(self, v => v.fill_buf()).into_inner()
    }
    fn consume(&mut self, amt: usize) {
        either!(self, v => v.consume(amt));
    }
}

impl<A, B> Seek for Either<A, B>
where
    A: Seek,
    B: Seek,
{
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, IoError> {
        either!(self, v => v.seek(pos)).into_inner()
    }
}

impl<A, B> Write for Either<A, B>
where
    A: Write,
    B: Write,
{
    fn write(&mut self, buf: &[u8]) -> Result<usize, IoError> {
        either!(self, v => v.write(buf)).into_inner()
    }
    fn flush(&mut self) -> Result<(), IoError> {
        either!(self, v => v.flush()).into_inner()
    }
}

impl<A: AsyncRead, B: AsyncRead> AsyncRead for Either<A, B> {}

impl<A, B> AsyncWrite for Either<A, B>
where
    A: AsyncWrite,
    B: AsyncWrite,
{
    fn shutdown(&mut self) -> Result<Async<()>, IoError> {
        either!(self, v => v.shutdown()).into_inner()
    }
}
