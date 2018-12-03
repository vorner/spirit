#[doc(hidden)]
pub use failure::Error;
#[doc(hidden)]
pub use serde::de::DeserializeOwned;
#[doc(hidden)]
pub use spirit::helpers::{CfgHelper, IteratedCfgHelper};
#[doc(hidden)]
pub use spirit::validation::Results as ValidationResults;
#[doc(hidden)]
pub use spirit::Builder;
#[doc(hidden)]
pub use structopt::StructOpt;

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

#[macro_export]
macro_rules! delegate_resource_traits {
    (delegate impl ListenLimits to $inner: ident on $type: ident;) => {
        impl<Inner: $crate::ListenLimits> $crate::ListenLimits for $type<Inner> {
            fn error_sleep(&self) -> ::std::time::Duration {
                self.$inner.error_sleep()
            }
        }
        fn max_conn(&self) -> usize {
            self.$inner.max_conn()
        }
    };
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
