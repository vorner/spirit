use std::borrow::Borrow;
use std::fmt::{Debug, Display};

use arc_swap::ArcSwap;
use serde::Deserialize;
use structopt::StructOpt;

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

impl<S, O, C, F> Helper<S, O, C> for F
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    F: FnOnce(Builder<S, O, C>) -> Builder<S, O, C>,
{
    fn apply(self, builder: Builder<S, O, C>) -> Builder<S, O, C> {
        self(builder)
    }
}

pub trait CfgHelper<S, O, C, Action>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
{
    fn apply<Extractor, Name>(
        extractor: Extractor,
        action: Action,
        name: Name,
        builder: Builder<S, O, C>,
    ) -> Builder<S, O, C>
    where
        Extractor: FnMut(&C) -> Self + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static;
}

pub trait IteratedCfgHelper<S, O, C, Action>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
{
    fn apply<Extractor, ExtractedIter, Name>(
        extractor: Extractor,
        action: Action,
        name: Name,
        builder: Builder<S, O, C>,
    ) -> Builder<S, O, C>
    where
        Self: Sized, // TODO: Why does rustc insist on this one?
        Extractor: FnMut(&C) -> ExtractedIter + Send + 'static,
        ExtractedIter: IntoIterator<Item = Self>,
        Name: Clone + Display + Send + Sync + 'static;
}

impl<S, O, C, Action, Iter, Target> CfgHelper<S, O, C, Action> for Iter
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    Iter: IntoIterator<Item = Target>,
    Target: IteratedCfgHelper<S, O, C, Action>,
{
    fn apply<Extractor, Name>(
        extractor: Extractor,
        action: Action,
        name: Name,
        builder: Builder<S, O, C>,
    ) -> Builder<S, O, C>
    where
        Extractor: FnMut(&C) -> Self + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        <Target as IteratedCfgHelper<S, O, C, Action>>::apply(extractor, action, name, builder)
    }
}

impl<S, O, C> Builder<S, O, C>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
{
    pub fn with<H: Helper<S, O, C>>(self, helper: H) -> Self {
        helper.apply(self)
    }

    pub fn config_helper<Cfg, Extractor, Action, Name>(
        self,
        extractor: Extractor,
        action: Action,
        name: Name,
    ) -> Self
    where
        Extractor: FnMut(&C) -> Cfg + Send + 'static,
        Cfg: CfgHelper<S, O, C, Action>,
        Name: Clone + Display + Send + Sync + 'static,
    {
        CfgHelper::apply(extractor, action, name, self)
    }
}
