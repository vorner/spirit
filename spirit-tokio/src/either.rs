use std::io::{BufRead, Error as IoError, Read, Seek, SeekFrom, Write};
use std::time::Duration;

use failure::Error;
use futures::{Async, Future, Poll, Sink, StartSend, Stream};
use spirit::validation::Results as ValidationResults;
use spirit::Builder;
use tokio::io::{AsyncRead, AsyncWrite};

use base_traits::{ExtraCfgCarrier, Name, ResourceConfig};
use net::{IntoIncoming, ListenLimits};

#[derive(Copy, Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(untagged)]
pub enum Either<A, B> {
    A(A),
    B(B),
}

impl<T> Either<T, T> {
    pub fn into_inner(self) -> T {
        match self {
            A(a) => a,
            B(b) => b,
        }
    }
}

use self::Either::{A, B};

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

impl<A, B> ListenLimits for Either<A, B>
where
    A: ListenLimits,
    B: ListenLimits,
{
    fn error_sleep(&self) -> Duration {
        either!(self, v => v.error_sleep()).into_inner()
    }
    fn max_conn(&self) -> usize {
        either!(self, v => v.max_conn()).into_inner()
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
