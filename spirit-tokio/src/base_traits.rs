//! Collection of traits used through this crate and by reverse dependencies.
//!
//! This module holds the traits that describe general interactions between things (most
//! importantly the [`ResourceConfig`] and [`ResourceConsumer`] traits) and their blanket
//! implementations.

// XXX: Remove this thing
/// A name of resource for logging.
///
/// This is a trait alias for string-like types. Specifically, [`String`] and [`&str`] implement
/// this trait.
pub trait Name: AsRef<str> + Clone + Send + Sync + 'static {}
impl<N> Name for N where N: AsRef<str> + Clone + Send + Sync + 'static {}

// XXX: This can go to spirit proper
// XXX: reqwest could have one too

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
