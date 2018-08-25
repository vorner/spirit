use std::borrow::Borrow;

use arc_swap::ArcSwap;

use super::Builder;

#[cfg(feature = "tokio-helpers")]
pub mod tokio;

#[cfg(not(feature = "tokio-helpers"))]
pub(crate) mod tokio {
    use std::marker::PhantomData;

    pub(crate) struct TokioGutsInner<T>(PhantomData<T>);

    impl<T> Default for TokioGutsInner<T> {
        fn default() -> Self {
            TokioGutsInner(PhantomData)
        }
    }

    pub(crate) struct TokioGuts<T>(PhantomData<T>);

    impl<T> From<TokioGutsInner<T>> for TokioGuts<T> {
        fn from(inner: TokioGutsInner<T>) -> Self {
            TokioGuts(inner.0)
        }
    }
}

pub trait Helper<S, O, C>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
{
    fn apply(self, builder: Builder<S, O, C>) -> Builder<S, O, C>;
}
