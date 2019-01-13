//! Macros to help implementing custom fragments.
//!
//! This module holds several macros that aim to help with boilerplate when implementing
//! [`ResourceConfig`] configuration fragments. Look into the [source code] for examples of use.
//!
//! The macros are not necessary for normal everyday use in daemons simply accepting configuration.
//!
//! There are also few re-exports (hidden from documentation). These are not expected to be used by
//! code, but only by the macros.
//!
//! [`ResourceConfig`]: ::ResourceConfig
//! [source code]: https://github.com/vorner/spirit/tree/master/spirit-tokio

#[doc(hidden)]
pub use failure::Error;
#[doc(hidden)]
pub use serde::de::DeserializeOwned;
#[doc(hidden)]
pub use spirit::extension::{CfgHelper, IteratedCfgHelper};
#[doc(hidden)]
pub use spirit::Builder;
#[doc(hidden)]
pub use structopt::StructOpt;

/// Implements the [`ExtraCfgCarrier`] trait by extracting a given field.
///
/// # Examples
///
/// ```rust
/// #[macro_use]
/// extern crate serde_derive;
/// #[macro_use]
/// extern crate spirit_tokio;
///
/// #[derive(Deserialize)]
/// struct MyConfig<ExtraCfg> {
///     _whatever_other_fields: String,
///     #[serde(flatten)]
///     extra_cfg: ExtraCfg,
/// }
///
/// extra_cfg_impl! {
///     MyConfig<ExtraCfg>::extra_cfg: ExtraCfg;
/// }
/// # fn main() {}
/// ```
///
/// [`ExtraCfgCarrier`]: ::ExtraCfgCarrier
#[macro_export]
macro_rules! extra_cfg_impl {
    ($(
         $type: ident<$($ty_param: ident),*>::$field: ident: $field_ty: ty;
    )*) => {
        $(
            impl<$($ty_param),*> $crate::ExtraCfgCarrier for $type<$($ty_param),*> {
                type Extra = $field_ty;
                fn extra(&self) -> &Self::Extra {
                    &self.$field
                }
            }
        )*
    }
}

/// Implements the [`CfgHelper`] and [`IteratedCfgHelper`] traits.
///
/// Given a type that already implements the [`ResourceConfig`], this adds the above two traits by
/// delegating the implementation to them.
///
/// [`CfgHelper`]: ::spirit::extension::CfgHelper
/// [`IteratedCfgHelper`]: ::spirit::extension::IteratedCfgHelper
#[macro_export]
macro_rules! cfg_helpers {
    (
        $(
        impl helpers for $type: ident < $($ty_param: ident),* >
        where
            $($ty_name: ident: $bound: tt $(+ $bounds: tt)*),*;
        )+
    ) => {
        $(
        impl<$($ty_param,)* O, C, Action> $crate::macros::CfgHelper<O, C, Action>
            for $type<$($ty_param),*>
        where
            Self: $crate::ResourceConfig<O, C>,
            Action: $crate::ResourceConsumer<Self, O, C>,
            C: $crate::macros::DeserializeOwned + Send + Sync + 'static,
            O: ::std::fmt::Debug + $crate::macros::StructOpt + Sync + Send + 'static,
            $( $ty_name: $bound $(+ $bounds)*, )*
        {
            fn apply<Extractor, Name>(
                extractor: Extractor,
                action: Action,
                name: Name,
                builder: $crate::macros::Builder<O, C>
            ) -> $crate::macros::Builder<O, C>
            where
                Extractor: FnMut(&C) -> Self + Send + 'static,
                Name: Clone + ::std::fmt::Display + Send + Sync + 'static
            {
                let name: ::std::sync::Arc<str> = ::std::sync::Arc::from(format!("{}", name));
                builder.with($crate::resource(extractor, action, name))
            }
        }

        impl<$($ty_param,)* O, C, Action> $crate::macros::IteratedCfgHelper<O, C, Action>
            for $type<$($ty_param),*>
        where
            Self: $crate::ResourceConfig<O, C>,
            Action: $crate::ResourceConsumer<Self, O, C>,
            C: $crate::macros::DeserializeOwned + Send + Sync + 'static,
            O: ::std::fmt::Debug + $crate::macros::StructOpt + Sync + Send + 'static,
            $( $ty_name: $bound $(+ $bounds)*, )*
        {
            fn apply<Extractor, ExtractedIter, Name>(
                extractor: Extractor,
                action: Action,
                name: Name,
                builder: $crate::macros::Builder<O, C>
            ) -> $crate::macros::Builder<O, C>
            where
                Extractor: FnMut(&C) -> ExtractedIter + Send + 'static,
                ExtractedIter: IntoIterator<Item = Self>,
                Name: Clone + ::std::fmt::Display + Send + Sync + 'static
            {
                let name: ::std::sync::Arc<str> = ::std::sync::Arc::from(format!("{}", name));
                builder.with($crate::resources(extractor, action, name))
            }
        }
        )+
    }
}

/// Delegates listed traits into a field.
///
/// If a trait is implemented for a field of a type, this allows for implementing it on the
/// container too by delegating it to the field. This is limited to these traits:
///
/// * [`ExtraCfgCarrier`]
/// * [`ResourceConfig`]
///
/// # Examples
///
/// ```rust
/// #[macro_use]
/// extern crate spirit_tokio;
///
/// // This one is trivial ‒ the real one would actually add some functionality or fields.
/// #[derive(Debug, Eq, PartialEq)]
/// struct Wrapper<Inner> {
///     inner: Inner,
/// }
///
/// delegate_resource_traits! {
///     delegate ExtraCfgCarrier, ResourceConfig to inner on Wrapper;
/// }
/// # fn main() { let _x = Wrapper { inner: () }; }
/// ```
///
/// [`ExtraCfgCarrier`]: ::base_traits::ExtraCfgCarrier
/// [`ResourceConfig`]: ::base_traits::ResourceConfig
#[macro_export]
macro_rules! delegate_resource_traits {
    (delegate impl ExtraCfgCarrier to $inner: ident on $type: ident;) => {
        impl<Inner: $crate::ExtraCfgCarrier> $crate::ExtraCfgCarrier for $type<Inner> {
            type Extra = Inner::Extra;
            fn extra(&self) -> &Self::Extra {
                self.$inner.extra()
            }
        }
    };
    (delegate impl ResourceConfig to $inner: ident on $type: ident;) => {
        impl<Inner, O, C> $crate::ResourceConfig<O, C> for $type<Inner>
        where
            Inner: $crate::ResourceConfig<O, C>,
        {
            type Seed = Inner::Seed;
            type Resource = Inner::Resource;
            fn create(&self, name: &str)
                -> ::std::result::Result<Self::Seed, $crate::macros::Error>
            {
                self.$inner.create(name)
            }
            fn fork(&self, seed: &Self::Seed, name: &str)
                -> ::std::result::Result<Self::Resource, $crate::macros::Error>
            {
                self.$inner.fork(seed, name)
            }
            fn scaled(&self, name: &str) -> (usize, $crate::macros::ValidationResults) {
                self.$inner.scaled(name)
            }
            fn is_similar(&self, other: &Self, name: &str) -> bool {
                self.$inner.is_similar(&other.$inner, name)
            }
            fn install<N: $crate::Name>(builder: $crate::macros::Builder<O, C>, name: &N)
                -> $crate::macros::Builder<O, C>
            {
                Inner::install(builder, name)
            }
        }
    };
    ($(delegate $($trait: ident),* to $inner: ident on $type: ident;)*) => {
        $( $(
            delegate_resource_traits! {
                delegate impl $trait to $inner on $type;
            }
        )* )*
    }
}
