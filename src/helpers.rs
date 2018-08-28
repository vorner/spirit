use std::any::TypeId;
use std::borrow::Borrow;
use std::fmt::{Debug, Display};

use arc_swap::ArcSwap;
use serde::Deserialize;
use structopt::StructOpt;

use super::Builder;

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
        trace!("Adding config helper for {}", name);
        CfgHelper::apply(extractor, action, name, self)
    }

    pub fn singleton<T: 'static>(&mut self) -> bool {
        self.singletons.insert(TypeId::of::<T>())
    }

    pub fn with<H: Helper<S, O, C>>(self, helper: H) -> Self {
        trace!("Adding a helper");
        helper.apply(self)
    }

    pub fn with_singleton<T: Helper<S, O, C> + 'static>(mut self, singleton: T) -> Self {
        if self.singleton::<T>() {
            self.with(singleton)
        } else {
            trace!("Singleton already exists");
            self
        }
    }
}
