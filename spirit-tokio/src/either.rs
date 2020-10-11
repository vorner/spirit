//! Support for alternative choices of configuration.

use std::future::Future;
use std::io::{Error as IoError, SeekFrom};
use std::pin::Pin;
use std::task::{Context, Poll};

use err_context::AnyError;
#[cfg(feature = "either")]
use either::Either as OtherEither;
#[cfg(feature = "futures")]
use futures_util::future::Either as FutEither;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use spirit::extension::Extensible;
use spirit::fragment::driver::{Comparable, Comparison, Driver, Instruction};
use spirit::fragment::{Fragment, Installer, Stackable, Transformation};
#[cfg(feature = "cfg-help")]
use structdoc::StructDoc;
use structopt::StructOpt;
use tokio::io::{AsyncBufRead, AsyncRead, AsyncSeek, AsyncWrite};
#[cfg(feature = "stream")]
use tokio::stream::Stream;

#[cfg(feature = "net")]
use crate::net::Accept;

// TODO: A lot of things here require Unpin. Intuitively, they just pin the inner field right away,
// so it should be fine to allow !Unpin types here too and use a bit of unsafe to do that. But we
// would need to allow unsafe ( :-( ) and prove that the intuition is right.

/// The [`Either`] type allows to wrap two similar [`Fragment`]s and let the user choose
/// which one will be used.
///
/// For example, if your server could run both on common TCP and unix domain stream sockets, you
/// could use the `Either<TcpListen, UnixListen>`. This fragment would then create resources of
/// type `Either<TcpListener, UnixListener>`.
///
/// Many traits are delegated through to one or the other instance inside (in case both implement
/// it). So, the above resource will implement the [`Accept`] trait that will accept
/// instances of `Either<TcpStream, UnixStream>`. These'll in turn implement [`AsyncRead`] and
/// [`AsyncWrite`], therefore can be handled uniformly just as connections.
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
/// If you need different parsing, you can use either a newtype or [remote derive].
///
/// # Other similar types
///
/// This is not the only `Either` type around. Unfortunately, none of the available ones was just
/// right for the use case here, so this crate rolls its own. But it provides [`From`]/[`Into`]
/// conversions between them, if the corresponding feature on this crate is enabled.
///
/// # More than two options
///
/// This allows only two variants. However, if you need more, it is possible to nest them and form
/// a tree.
///
/// # Drawbacks
///
/// Due to the complexity of implementation, the [`Fragment`] is implemented for either only if
/// both variants are [`Fragment`]s with simple enough [`Driver`]s (drivers that don't sub-divide
/// their [`Fragment`]s). Therefore, `Vec<Either<TcpListen, UnixListen>>` will work, but
/// `Either<Vec<TcpListen>, Vec<UnixListen>>` will not.
///
/// This is an implementation limitation and may be lifted in the future (PRs are welcome).
///
/// # Examples
///
/// ```rust
/// use std::sync::Arc;
///
/// use serde::Deserialize;
/// use spirit::{AnyError, Empty, Pipeline, Spirit};
/// use spirit::prelude::*;
/// #[cfg(unix)]
/// use spirit_tokio::either::Either;
/// use spirit_tokio::handlers::PerConnection;
/// use spirit_tokio::net::TcpListen;
/// #[cfg(unix)]
/// use spirit_tokio::net::unix::UnixListen;
/// use tokio::prelude::*;
///
/// // If we want to work on systems that don't have unix domain sockets...
///
/// #[cfg(unix)]
/// type Listener = Either<TcpListen, UnixListen>;
/// #[cfg(not(unix))]
/// type Listener = TcpListen;
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
///     fn listen(&self) -> Vec<Listener> {
///         self.listening_socket.clone()
///     }
/// }
///
/// async fn handle_connection<C: AsyncWrite + Unpin>(mut conn: C) -> Result<(), AnyError> {
///     conn.write_all(b"hello world").await?;
///     conn.shutdown().await?;
///     Ok(())
/// }
///
/// fn main() {
///     let handler = PerConnection(|conn, _cfg: &_| async {
///         if let Err(e) = handle_connection(conn).await {
///             eprintln!("Error: {}", e);
///         }
///     });
///     Spirit::<Empty, Config>::new()
///         .config_defaults(DEFAULT_CONFIG)
///         .with(Pipeline::new("listen").extract_cfg(Config::listen).transform(handler))
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
/// [`TcpListen`]: crate::TcpListen
/// [`UnixListen`]: crate::net::unix::UnixListen
#[derive(Copy, Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
#[serde(untagged)]
pub enum Either<A, B> {
    #[allow(missing_docs)]
    A(A),
    #[allow(missing_docs)]
    B(B),
}

use self::Either::{A, B};

macro_rules! either_unwrap {
    ($either: expr, $pat: pat => $res: expr) => {
        match $either {
            A($pat) => $res,
            B($pat) => $res,
        }
    };
}

impl<T> Either<T, T> {
    /// Extracts the inner value in case both have the same type.
    ///
    /// Sometimes, a series of operations produces an `Either` with both types the same. In such
    /// case, `Either` plays no role anymore and this method can be used to get to the inner value.
    pub fn into_inner(self) -> T {
        either_unwrap!(self, v => v)
    }
}

#[cfg(feature = "futures")]
impl<A, B> From<FutEither<A, B>> for Either<A, B> {
    fn from(e: FutEither<A, B>) -> Self {
        match e {
            FutEither::Left(a) => A(a),
            FutEither::Right(b) => B(b),
        }
    }
}

#[cfg(feature = "futures")]
impl<A, B> Into<FutEither<A, B>> for Either<A, B> {
    fn into(self) -> FutEither<A, B> {
        match self {
            A(a) => FutEither::Left(a),
            B(b) => FutEither::Right(b),
        }
    }
}

#[cfg(feature = "either")]
impl<A, B> From<OtherEither<A, B>> for Either<A, B> {
    fn from(e: OtherEither<A, B>) -> Self {
        match e {
            OtherEither::Left(a) => A(a),
            OtherEither::Right(b) => B(b),
        }
    }
}

#[cfg(feature = "either")]
impl<A, B> Into<OtherEither<A, B>> for Either<A, B> {
    fn into(self) -> OtherEither<A, B> {
        match self {
            A(a) => OtherEither::Left(a),
            B(b) => OtherEither::Right(b),
        }
    }
}

#[cfg(feature = "net")]
impl<A, B> Accept for Either<A, B>
where
    A: Accept,
    B: Accept,
{
    type Connection = Either<A::Connection, B::Connection>;
    fn poll_accept(&mut self, ctx: &mut Context) -> Poll<Result<Self::Connection, IoError>> {
        match self {
            A(a) => a.poll_accept(ctx).map(|r| r.map(A)),
            B(b) => b.poll_accept(ctx).map(|r| r.map(B)),
        }
    }
}

impl<A, B> Future for Either<A, B>
where
    A: Future + Unpin,
    B: Future + Unpin,
{
    type Output = Either<A::Output, B::Output>;
    fn poll(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Self::Output> {
        match self.get_mut() {
            A(a) => Pin::new(a).poll(ctx).map(A),
            B(b) => Pin::new(b).poll(ctx).map(B),
        }
    }
}

#[cfg(feature = "stream")]
impl<A, B> Stream for Either<A, B>
where
    A: Stream + Unpin,
    B: Stream + Unpin,
{
    type Item = Either<A::Item, B::Item>;
    fn poll_next(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Option<Self::Item>> {
        match self.get_mut() {
            A(a) => Pin::new(a).poll_next(ctx).map(|i| i.map(A)),
            B(b) => Pin::new(b).poll_next(ctx).map(|i| i.map(B)),
        }
    }
}

impl<A, B> AsyncBufRead for Either<A, B>
where
    A: AsyncBufRead + Unpin,
    B: AsyncBufRead + Unpin,
{
    fn poll_fill_buf(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<&[u8], IoError>> {
        either_unwrap!(self.get_mut(), v => Pin::new(v).poll_fill_buf(ctx))
    }
    fn consume(self: Pin<&mut Self>, amt: usize) {
        either_unwrap!(self.get_mut(), v => Pin::new(v).consume(amt))
    }
}

impl<A, B> AsyncRead for Either<A, B>
where
    A: AsyncRead + Unpin,
    B: AsyncRead + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        ctx: &mut Context,
        buf: &mut [u8],
    ) -> Poll<Result<usize, IoError>> {
        either_unwrap!(self.get_mut(), v => Pin::new(v).poll_read(ctx, buf))
    }
}

impl<A, B> AsyncSeek for Either<A, B>
where
    A: AsyncSeek + Unpin,
    B: AsyncSeek + Unpin,
{
    fn start_seek(
        self: Pin<&mut Self>,
        ctx: &mut Context,
        position: SeekFrom,
    ) -> Poll<Result<(), IoError>> {
        either_unwrap!(self.get_mut(), v => Pin::new(v).start_seek(ctx, position))
    }
    fn poll_complete(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Result<u64, IoError>> {
        either_unwrap!(self.get_mut(), v => Pin::new(v).poll_complete(ctx))
    }
}

impl<A, B> AsyncWrite for Either<A, B>
where
    A: AsyncWrite + Unpin,
    B: AsyncWrite + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        ctx: &mut Context,
        buf: &[u8],
    ) -> Poll<Result<usize, IoError>> {
        either_unwrap!(self.get_mut(), v => Pin::new(v).poll_write(ctx, buf))
    }

    fn poll_flush(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Result<(), IoError>> {
        either_unwrap!(self.get_mut(), v => Pin::new(v).poll_flush(ctx))
    }

    fn poll_shutdown(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Result<(), IoError>> {
        either_unwrap!(self.get_mut(), v => Pin::new(v).poll_shutdown(ctx))
    }
}

impl<A, B> Stackable for Either<A, B>
where
    A: Stackable,
    B: Stackable,
{ }

impl<A, B, AR, BR> Comparable<Either<AR, BR>> for Either<A, B>
where
    A: Comparable<AR>,
    B: Comparable<BR>,
{
    fn compare(&self, rhs: &Either<AR, BR>) -> Comparison {
        match (self, rhs) {
            (Either::A(s), Either::A(r)) => s.compare(r),
            (Either::B(s), Either::B(r)) => s.compare(r),
            _ => Comparison::Dissimilar,
        }
    }
}

impl<A, B> Fragment for Either<A, B>
where
    A: Fragment,
    A::Driver: Driver<A, SubFragment = A>,
    B: Fragment,
    B::Driver: Driver<B, SubFragment = B>,
{
    type Driver = EitherDriver<A, B>;
    type Installer = EitherInstaller<A::Installer, B::Installer>;
    type Seed = Either<A::Seed, B::Seed>;
    type Resource = Either<A::Resource, B::Resource>;
    fn make_seed(&self, name: &'static str) -> Result<Self::Seed, AnyError> {
        match self {
            Either::A(a) => Ok(Either::A(a.make_seed(name)?)),
            Either::B(b) => Ok(Either::B(b.make_seed(name)?)),
        }
    }
    fn make_resource(
        &self,
        seed: &mut Self::Seed,
        name: &'static str,
    ) -> Result<Self::Resource, AnyError> {
        match (self, seed) {
            (Either::A(a), Either::A(sa)) => Ok(Either::A(a.make_resource(sa, name)?)),
            (Either::B(b), Either::B(sb)) => Ok(Either::B(b.make_resource(sb, name)?)),
            _ => unreachable!("Seed vs. fragment mismatch"),
        }
    }
}

/// An [`Installer`] for [`Either`] [`Resource`]s.
///
/// This wraps two distinct installers so it can install resources that are installable by one or
/// the other.
///
/// Note that this to work, *both* installers need to exist at the same time (as opposed to the
/// resource where one or the other is in existence).
///
/// [`Resource`]: Fragment::Resource
#[derive(Debug, Default)]
pub struct EitherInstaller<A, B>(A, B);

impl<A, B, RA, RB, O, C> Installer<Either<RA, RB>, O, C> for EitherInstaller<A, B>
where
    A: Installer<RA, O, C>,
    B: Installer<RB, O, C>,
{
    type UninstallHandle = Either<A::UninstallHandle, B::UninstallHandle>;
    fn install(&mut self, resource: Either<RA, RB>, name: &'static str) -> Self::UninstallHandle {
        match resource {
            Either::A(ra) => Either::A(self.0.install(ra, name)),
            Either::B(rb) => Either::B(self.1.install(rb, name)),
        }
    }
    fn init<E: Extensible<Opts = O, Config = C, Ok = E>>(
        &mut self,
        builder: E,
        name: &'static str,
    ) -> Result<E, AnyError>
    where
        E::Config: DeserializeOwned + Send + Sync + 'static,
        E::Opts: StructOpt + Send + Sync + 'static,
    {
        let builder = self.0.init(builder, name)?;
        let builder = self.1.init(builder, name)?;
        Ok(builder)
    }
}

/// A [`Driver`] used for [`Either`] [`Fragment`]s.
///
/// This switches between driving the variants ‒ if the fragment changes from one variant to
/// another, the old driver is dropped and new one created for the other one. If the variant stays
/// the same, driving is delegated to the existing driver.
///
/// Note that there are limitations to what this driver implementation ‒ see the
/// [`Either` drawbacks](struct.Either.html#drawbacks).
#[derive(Debug)]
pub struct EitherDriver<A, B>
where
    A: Fragment,
    B: Fragment,
{
    driver: Either<A::Driver, B::Driver>,
    new_driver: Option<Either<A::Driver, B::Driver>>,
}

impl<A, B> Default for EitherDriver<A, B>
where
    A: Fragment,
    A::Driver: Default,
    B: Fragment,
{
    fn default() -> Self {
        EitherDriver {
            driver: Either::A(Default::default()),
            new_driver: None,
        }
    }
}

// TODO: This is a bit limiting
impl<A, B> Driver<Either<A, B>> for EitherDriver<A, B>
where
    A: Fragment,
    A::Driver: Driver<A, SubFragment = A> + Default,
    B: Fragment,
    B::Driver: Driver<B, SubFragment = B> + Default,
{
    type SubFragment = Either<A, B>;
    fn instructions<T, I>(
        &mut self,
        fragment: &Either<A, B>,
        transform: &mut T,
        name: &'static str,
    ) -> Result<Vec<Instruction<T::OutputResource>>, Vec<AnyError>>
    where
        T: Transformation<<Self::SubFragment as Fragment>::Resource, I, Self::SubFragment>,
    {
        assert!(self.new_driver.is_none(), "Unclosed transaction");

        // Shape adaptor for the transformation ‒ we need to first wrap in A or B before feeding it
        // into the either-transformation.
        //
        // Note that due to the lifetimes, we cache the outer fragment, not the inner part that the
        // transformation gets. It should be the same one.
        //
        // T: Transformation on the either
        // F: The original configuration fragment (eg Either<A, B>)
        // W: Wrapping function (Either::A or Either::B)
        struct Wrap<'a, T, F, W>(&'a mut T, &'a F, W);

        impl<'a, T, I, Fi, Fo, W> Transformation<Fi::Resource, I, Fi> for Wrap<'a, T, Fo, W>
        where
            Fi: Fragment,
            Fi::Driver: Driver<Fi, SubFragment = Fi>,
            Fo: Fragment,
            W: Fn(Fi::Resource) -> Fo::Resource,
            T: Transformation<Fo::Resource, I, Fo>,
        {
            type OutputResource = T::OutputResource;
            type OutputInstaller = T::OutputInstaller;
            fn installer(&mut self, in_installer: I, name: &'static str) -> T::OutputInstaller {
                self.0.installer(in_installer, name)
            }
            fn transform(
                &mut self,
                resource: Fi::Resource,
                _fragment: &Fi,
                name: &'static str,
            ) -> Result<Self::OutputResource, AnyError> {
                self.0.transform((self.2)(resource), self.1, name)
            }
        }

        match (&mut self.driver, fragment) {
            (Either::A(da), Either::A(a)) => {
                da.instructions(a, &mut Wrap(transform, fragment, Either::A), name)
            }
            (Either::B(db), Either::B(b)) => {
                db.instructions(b, &mut Wrap(transform, fragment, Either::B), name)
            }
            (Either::B(_), Either::A(a)) => {
                let mut da = A::Driver::default();
                let result = da.instructions(a, &mut Wrap(transform, fragment, Either::A), name);
                self.new_driver = Some(Either::A(da));
                result
            }
            (Either::A(_), Either::B(b)) => {
                let mut db = B::Driver::default();
                let result = db.instructions(b, &mut Wrap(transform, fragment, Either::B), name);
                self.new_driver = Some(Either::B(db));
                result
            }
        }
    }
    fn confirm(&mut self, name: &'static str) {
        if let Some(new) = self.new_driver.take() {
            self.driver = new;
        }
        either_unwrap!(&mut self.driver, v => v.confirm(name))
    }
    fn abort(&mut self, name: &'static str) {
        if self.new_driver.is_some() {
            self.new_driver.take();
        } else {
            either_unwrap!(&mut self.driver, v => v.abort(name))
        }
    }
    fn maybe_cached(&self, fragment: &Either<A, B>, name: &'static str) -> bool {
        match (&self.driver, fragment) {
            (Either::A(da), Either::A(a)) => da.maybe_cached(a, name),
            (Either::B(db), Either::B(b)) => db.maybe_cached(b, name),
            _ => false,
        }
    }
}
