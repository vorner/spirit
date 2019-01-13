use std::collections::{BTreeSet, BinaryHeap, HashSet, LinkedList};
use std::hash::{BuildHasher, Hash};

use failure::Error;

use self::driver::{Driver, RefDriver, SeqDriver};
use crate::extension::Extensible;

pub mod driver;
pub mod pipeline;

// TODO: Add logging/trace logs?
// TODO: Use ValidationResult instead?

pub trait Installer<Resource, O, C>: Default {
    type UninstallHandle: Send + 'static;
    // TODO: Add names here too
    fn install(&mut self, resource: Resource) -> Self::UninstallHandle;
    fn init<B: Extensible<Opts = O, Config = C, Ok = B>>(
        &mut self,
        builder: B,
    ) -> Result<B, Error> {
        Ok(builder)
    }
}

#[derive(Debug, Default)]
pub struct SeqInstaller<Slave> {
    slave: Slave,
}

impl<Resource, O, C, Slave> Installer<Resource, O, C> for SeqInstaller<Slave>
where
    Resource: IntoIterator,
    Slave: Installer<Resource::Item, O, C>,
{
    type UninstallHandle = Vec<Slave::UninstallHandle>;
    fn install(&mut self, resource: Resource) -> Self::UninstallHandle {
        resource
            .into_iter()
            .map(|r| self.slave.install(r))
            .collect()
    }
    fn init<B: Extensible<Opts = O, Config = C, Ok = B>>(
        &mut self,
        builder: B,
    ) -> Result<B, Error> {
        self.slave.init(builder)
    }
}

// Marker trait...
pub trait Stackable {}

pub trait Fragment: Sized {
    type Driver: Driver<Self> + Default;
    type Installer: Default;
    type Seed;
    type Resource;
    const RUN_BEFORE_CONFIG: bool = false;
    fn make_seed(&self, name: &str) -> Result<Self::Seed, Error>;
    fn make_resource(&self, seed: &mut Self::Seed, name: &str) -> Result<Self::Resource, Error>;
    fn create(&self, name: &str) -> Result<Self::Resource, Error> {
        let mut seed = self.make_seed(name)?;
        self.make_resource(&mut seed, name)
    }
    fn init<B: Extensible<Ok = B>>(builder: B, _: &str) -> Result<B, Error> {
        Ok(builder)
    }
}

impl<'a, F> Fragment for &'a F
where
    F: Fragment,
{
    type Driver = RefDriver<F::Driver>;
    type Installer = F::Installer;
    type Seed = F::Seed;
    type Resource = F::Resource;
    const RUN_BEFORE_CONFIG: bool = F::RUN_BEFORE_CONFIG;
    fn make_seed(&self, name: &str) -> Result<Self::Seed, Error> {
        F::make_seed(*self, name)
    }
    fn make_resource(&self, seed: &mut Self::Seed, name: &str) -> Result<Self::Resource, Error> {
        F::make_resource(*self, seed, name)
    }
    fn init<B: Extensible<Ok = B>>(builder: B, name: &str) -> Result<B, Error> {
        F::init(builder, name)
    }
}

// TODO: Export the macro for other containers?
// TODO: The where-* should be where-?
macro_rules! fragment_for_seq {
    ($container: ident<$base: ident $(, $extra: ident)*> $(where $($bounds: tt)+)*) => {
        impl<$base: Fragment + Stackable + 'static $(, $extra)*> Fragment
            for $container<$base $(, $extra)*>
        $(
            where
            $($bounds)+
        )*
        {
            type Driver = SeqDriver<$base, $base::Driver>;
            type Installer = SeqInstaller<$base::Installer>;
            type Seed = Vec<$base::Seed>;
            type Resource = Vec<$base::Resource>;
            const RUN_BEFORE_CONFIG: bool = $base::RUN_BEFORE_CONFIG;
            fn make_seed(&self, name: &str) -> Result<Self::Seed, Error> {
                self.iter().map(|i| i.make_seed(name)).collect()
            }
            fn make_resource(&self, seed: &mut Self::Seed, name: &str)
                -> Result<Self::Resource, Error>
            {
                self.iter()
                    .zip(seed)
                    .map(|(i, s)| i.make_resource(s, name))
                    .collect()
            }
            fn init<B: Extensible<Ok = B>>(builder: B, name: &str) -> Result<B, Error> {
                $base::init(builder, name)
            }
        }
    }
}

fragment_for_seq!(Vec<T>);
fragment_for_seq!(BTreeSet<T>);
fragment_for_seq!(LinkedList<T>);
fragment_for_seq!(Option<T>);
fragment_for_seq!(BinaryHeap<T> where T: Ord);
fragment_for_seq!(HashSet<T, S> where T: Eq + Hash, S: BuildHasher);

// TODO: Generics
#[macro_export]
macro_rules! simple_fragment {
    (impl Fragment for $ty: ty {
        type Resource = $resource: ty;
        type Installer = $installer: ty;
        fn create(&$self: tt, $name: tt: &str) -> $result: ty $block: block
    }) => {
        $crate::simple_fragment! {
            impl Fragment for $ty {
                type Driver = $crate::fragment::driver::TrivialDriver;
                type Resource = $resource;
                type Installer = $installer;
                fn create(&$self, $name: &str) -> $result $block
            }
        }
    };
    (impl Fragment for $ty: ty {
        type Driver = $driver: ty;
        type Resource = $resource: ty;
        type Installer = $installer: ty;
        fn create(&$self: tt, $name: tt: &str) -> $result: ty $block: block
    }) => {
        impl $crate::fragment::Fragment for $ty {
            type Driver = $driver;
            type Resource = $resource;
            type Installer = $installer;
            type Seed = ();
            fn make_seed(&self, _: &str) -> Result<(), Error> {
                Ok(())
            }
            fn make_resource(&$self, _: &mut (), $name: &str) -> $result $block
        }
    }
}

// TODO: How do we stack maps, etc?
// TODO: Arcs, Rcs, Mutexes, refs, ...

pub trait Extractor<'a, O, C> {
    type Fragment: Fragment + 'a;
    fn extract(&mut self, opts: &'a O, config: &'a C) -> Self::Fragment;
}

impl<'a, O: 'a, C: 'a, F, R> Extractor<'a, O, C> for F
where
    F: FnMut(&'a O, &'a C) -> R,
    R: Fragment + 'a,
{
    type Fragment = R;
    fn extract(&mut self, opts: &'a O, config: &'a C) -> R {
        self(opts, config)
    }
}

pub trait Transformation<InputResource, InputInstaller, SubFragment> {
    type OutputResource: 'static;
    type OutputInstaller;
    fn installer(&mut self, installer: InputInstaller, name: &str) -> Self::OutputInstaller;
    fn transform(
        &mut self,
        resource: InputResource,
        fragment: &SubFragment,
        name: &str,
    ) -> Result<Self::OutputResource, Error>;
}
