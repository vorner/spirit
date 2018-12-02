use std::fmt::{Debug, Display};
use std::sync::Arc;

use failure::Error;
use futures::future::{Future, IntoFuture};
use spirit::validation::Results as ValidationResults;
use spirit::{Builder, Spirit};

pub trait Name: AsRef<str> + Clone + Send + Sync + 'static {}
impl<N> Name for N where N: AsRef<str> + Clone + Send + Sync + 'static {}

// TODO: Move to spirit itself? Might be useful there.
pub trait ExtraCfgCarrier {
    type Extra;
    fn extra(&self) -> &Self::Extra;
}

// TODO: Are all these trait bounds necessary?
pub trait ResourceConfig<O, C>:
    Debug + ExtraCfgCarrier + Sync + Send + PartialEq + 'static
{
    type Seed: Send + Sync + 'static;
    type Resource: Send + 'static;
    fn create(&self, name: &str) -> Result<Self::Seed, Error>;
    fn fork(&self, seed: &Self::Seed, name: &str) -> Result<Self::Resource, Error>;
    fn scaled(&self, _name: &str) -> (usize, ValidationResults) {
        (1, ValidationResults::new())
    }
    fn is_similar(&self, other: &Self, _name: &str) -> bool {
        self == other
    }
    fn install<N: Name>(builder: Builder<O, C>, _name: &N) -> Builder<O, C> {
        builder
    }
}

pub trait ResourceConsumer<Config, O, C>: Sync + Send + 'static
where
    Config: ResourceConfig<O, C>,
{
    type Error: Display;
    type Future: Future<Item = (), Error = Self::Error> + Send + 'static;
    fn build_future(
        &self,
        spirit: &Arc<Spirit<O, C>>,
        config: &Arc<Config>,
        resource: Config::Resource,
        name: &str,
    ) -> Self::Future;
    fn install<N: Name>(builder: Builder<O, C>, _name: &N) -> Builder<O, C> {
        builder
    }
}

impl<F, Config, O, C, R> ResourceConsumer<Config, O, C> for F
where
    F: Fn(&Arc<Spirit<O, C>>, &Arc<Config>, Config::Resource, &str) -> R + Sync + Send + 'static,
    Config: ResourceConfig<O, C>,
    R: IntoFuture<Item = ()>,
    R::Error: Display,
    R::Future: Sync + Send + 'static,
{
    type Error = R::Error;
    type Future = R::Future;
    fn build_future(
        &self,
        spirit: &Arc<Spirit<O, C>>,
        config: &Arc<Config>,
        resource: Config::Resource,
        name: &str,
    ) -> R::Future {
        self(spirit, config, resource, name).into_future()
    }
}
