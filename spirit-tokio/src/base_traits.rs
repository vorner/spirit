//! Collection of traits used through this crate and by reverse dependencies.
//!
//! This module holds the traits that describe general interactions between things (most
//! importantly the [`ResourceConfig`] and [`ResourceConsumer`] traits) and their blanket
//! implementations.

use std::fmt::{Debug, Display};
use std::sync::Arc;

use failure::Error;
use futures::future::{Future, IntoFuture};
use spirit::validation::Results as ValidationResults;
use spirit::{Builder, Spirit};

/// A name of resource for logging.
///
/// This is a trait alias for string-like types. Specifically, [`String`] and [`&str`] implement
/// this trait.
pub trait Name: AsRef<str> + Clone + Send + Sync + 'static {}
impl<N> Name for N where N: AsRef<str> + Clone + Send + Sync + 'static {}

/// A [`ResourceConfig`] also carrying some custom extra configuration.
///
/// Many [`ResourceConfig`]s allow adding some custom extra configuration that doesn't influence
/// the creation of the resource, but is merely bundled to it. As the config fragment is passed to
/// the [`ResourceConsumer`], the consumer may make use of it. But because most of the insides of
/// the [`ResourceConfig`]s are private and it makes sense to allow writing the consumers in a
/// generic manner, this allows extracting the extra configuration out of the fragment.
///
/// It is a good idea to delegate the trait in case you implement a wrapper [`ResourceConfig`]. See
/// the [`macros`] module.
///
/// [`macros`]: ::macros
pub trait ExtraCfgCarrier {
    /// The extra config type carried inside.
    type Extra;

    /// Getter for the extra config.
    fn extra(&self) -> &Self::Extra;
}

/// The resource config.
///
/// This represents a fragment that can be added into the configuration structure. It describes
/// some kind of resource ‒ for example a TCP listener.
///
/// It then knows how to build the corresponding resource.
///
/// Some are „bottom“ building blocks, others are wrappers around some other resource configs to
/// add more configuration to them and specialize the resource. Note that when implementing a
/// wrapper, you might want to delegate further traits and also delegate *all* the methods (even
/// the provided ones).
///
/// # Creation process
///
/// The process has multiple stages. At the first stage a single [`Seed`] is [`create`]d. This seed
/// is then [`fork`]ed as many times as needed to satisfy the requirements of [`scaled`]. Both
/// [`create`] and [`fork`] are called within the context of the [configuration validator].
/// Therefore, they are allowed to block. Error from either aborts loading of the configuration.
///
/// Then the resources are sent to the runtime, passed to the [`ResourceConsumer`] and spawned.
///
/// A caching is performed between reconfigurations. If the old and new configuration is the same
/// (using equality), nothing is done. If it is different, but [`is_similar`] returns true, the
/// original seed is preserved. The old futures created by the [`ResourceConsumer`] are dropped
/// (canceled) and new resources are forked and again paired. This allows to pass new configuration
/// to them or to spawn a different number of them.
///
/// If it is not similar at all, the both the old futures and seed are dropped and the process
/// starts anew.
///
/// # The type parameters
///
/// They are the same type parameters as the `Spirit` uses. Most resource configs don't care about
/// them, but it allows creating some that are specific for the given [`Spirit`].
///
/// [`Seed`]: ResourceConfig::Seed
/// [`create`]: ResourceConfig::create
/// [`fork`]: ResourceConfig::fork
/// [`scaled`]: ResourceConfig::scaled
/// [`is_similar`]: ResourceConfig::is_similar
/// [`Spirit`]: ::spirit::Spirit
// TODO: Are all these trait bounds necessary?
pub trait ResourceConfig<O, C>:
    Debug + ExtraCfgCarrier + Sync + Send + PartialEq + 'static
{
    /// The intermediate product that is cached.
    ///
    /// This is cached in a single instance and then forked to scale to whatever amount is needed
    /// by [`scaled`].
    ///
    /// It is cached and reused to create resources that are similar but not entirely the same (see
    /// [`is_similar`]).
    ///
    /// [`scaled`]: ResourceConfig::scaled
    /// [`is_similar`]: ResourceConfig::is_similar
    type Seed: Send + Sync + 'static;

    /// The final produced resource.
    type Resource: Send + 'static;

    /// The first stage of resource construction.
    ///
    /// This creates the [`Seed`].
    ///
    /// [`Seed`]: ResourceConfig::Seed
    fn create(&self, name: &str) -> Result<Self::Seed, Error>;

    /// This creates the [`Resource`] from [`Seed`].
    ///
    /// Note that it may be called multiple times with the same seed.
    ///
    /// [`Resource`]: ResourceConfig::Resource
    /// [`Seed`]: ResourceConfig::Seed
    fn fork(&self, seed: &Self::Seed, name: &str) -> Result<Self::Resource, Error>;

    /// Returns how many instances of the resource should be present.
    ///
    /// The default implementation returns `1` instance.
    ///
    /// The [`ValidationResults`] allow for issuing warnings and errors about the configuration.
    fn scaled(&self, _name: &str) -> (usize, ValidationResults) {
        (1, ValidationResults::new())
    }

    /// Compares two configurations for similarity.
    ///
    /// In case the configurations are similar but not the same, the [`Seed`] is reused but new
    /// [`Resource`]s are created and sent to the corresponding [`ResourceConsumer`].
    ///
    /// This allows, for example, keeping the same TCP socket when the port doesn't change but to
    /// change the number of allowed concurrent connections.
    ///
    /// The default implementation compares for equality ‒ only directly equal configurations are
    /// considered similar by default.
    ///
    /// [`Seed`]: ResourceConfig::Seed
    /// [`Resource`]: ResourceConfig::Resource
    fn is_similar(&self, other: &Self, _name: &str) -> bool {
        self == other
    }

    /// A hook called when the resource config is plugged into a [`Builder`].
    ///
    /// This is usually left alone (the default implementation does nothing). This, however, allows
    /// interacting with the builder and for example doing further [validation] (or modification) of
    /// the config. Note that this is called before the validator that builds the resource, so any
    /// possible modifications will take effect.
    ///
    /// [validation]: Builder::config_validator
    fn install<N: Name>(builder: Builder<O, C>, _name: &N) -> Builder<O, C> {
        builder
    }
}

/// The part that gets a resource and builds a future out of it.
///
/// This is the place where application specific code goes. It is given the created resource (or
/// resources ‒ see the details of construction on [`ResourceConfig`]. It then produces a future
/// that gets spawned onto a runtime and is dropped when no longer needed.
///
/// A closure taking the right parameters (corresponding to [`build_future`]) implements this
/// trait. In application code, you usually provide either a closure, or closure wrapped by some
/// decorator function (like [`per_connection`]).
///
/// # Warning
///
/// Errors returned from here are *only logged*. If some further action is needed to be taken as a
/// result of an error (aborting the program, calling the admin…), it needs to be done as part of
/// the returned future already.
///
/// [`build_future`]: ResourceConsumer::build_future
/// [`per_connection`]: ::per_connection
pub trait ResourceConsumer<Config, O, C>: Sync + Send + 'static
where
    Config: ResourceConfig<O, C>,
{
    /// The error returned by the future.
    ///
    /// Note that the error is only logged. If some action needs to be taken as part of the error
    /// handling, it is the responsibility of the implementor of this trait to already do so.
    type Error: Display;

    /// The type of the future created.
    type Future: Future<Item = (), Error = Self::Error> + Send + 'static;

    /// Turns the provided resource into a future that'll get spawned onto a runtime.
    ///
    /// # Parameters
    ///
    /// * `spirit`: The global spirit object managing the application.
    /// * `config`: The config that led to creation of this resource and implements the
    ///   [`ResourceConfig`] trait. In case of the fragments in this crate, this is mostly useful
    ///   only if it carries some extra configuration (specified by a type parameter). See the
    ///   [`ExtraCfgCarrier`] trait. User-supplied [`ResourceConfig`]s may of course offer
    ///   additional interface to interact with them (like having public fields).
    /// * `resource`: The actual resource to use.
    /// * `name`: The name of the resource as it should appear in logs.
    fn build_future(
        &self,
        spirit: &Arc<Spirit<O, C>>,
        config: &Arc<Config>,
        resource: Config::Resource,
        name: &str,
    ) -> Self::Future;

    /// A hook called when this gets plugged into the [`Builder`].
    ///
    /// The provided implementation does nothing (same as implementation for closures). This,
    /// however, allows for further interaction with the [`Builder`], like installing additional
    /// [`config_validator`] to validate (or modify) the configuration. It is called before
    /// installation of the validator that manages creation of the resource.
    ///
    /// [`config_validator`]: Builder::config_validator
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
