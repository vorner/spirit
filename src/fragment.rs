use std::collections::{BTreeSet, BinaryHeap, HashMap, HashSet, LinkedList};
use std::fmt::Debug;
use std::hash::{BuildHasher, Hash};
use std::iter;
use std::marker::PhantomData;
use std::mem;
use std::sync::Arc;

use either::Either;
use failure::Error;
use log::trace;
use parking_lot::Mutex;

use crate::extension::{Extensible, Extension};
use crate::validation::{Result as ValidationResult, Results as ValidationResults};

// TODO: Add logging/trace logs?
// TODO: Use ValidationResult instead?

#[derive(Debug)]
pub struct IdGen(u128);

impl IdGen {
    fn new() -> Self {
        IdGen(1)
    }
}

impl Default for IdGen {
    fn default() -> Self {
        Self::new()
    }
}

impl Iterator for IdGen {
    type Item = CacheId;
    fn next(&mut self) -> Option<CacheId> {
        let id = self.0;
        self.0 = self
            .0
            .checked_add(1)
            .expect("WTF? Run out of 128bit cache IDs!?");
        Some(CacheId(id))
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct CacheId(u128);

impl CacheId {
    fn dummy() -> Self {
        CacheId(0)
    }
}

pub enum CacheInstruction<Resource> {
    DropAll,
    DropSpecific(CacheId),
    Install { id: CacheId, resource: Resource },
}

pub trait Driver<F: Fragment> {
    type SubFragment: Fragment;
    fn instructions<T, I>(
        &mut self,
        fragment: &F,
        transform: &mut T,
        name: &str,
    ) -> Result<Vec<CacheInstruction<T::OutputResource>>, Vec<Error>>
    where
        T: Transformation<<Self::SubFragment as Fragment>::Resource, I, Self::SubFragment>;
    fn confirm(&mut self);
    fn abort(&mut self);
    fn maybe_cached(&self, frament: &F) -> bool;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TrivialDriver;

impl<F: Fragment> Driver<F> for TrivialDriver {
    type SubFragment = F;
    fn instructions<T, I>(
        &mut self,
        fragment: &F,
        transform: &mut T,
        name: &str,
    ) -> Result<Vec<CacheInstruction<T::OutputResource>>, Vec<Error>>
    where
        T: Transformation<F::Resource, I, F>,
    {
        let resource = fragment
            .create(name)
            .and_then(|r| transform.transform(r, fragment, name))
            .map_err(|e| vec![e])?;
        Ok(vec![
            CacheInstruction::DropAll,
            CacheInstruction::Install {
                id: CacheId::dummy(),
                resource,
            },
        ])
    }
    fn confirm(&mut self) {}
    fn abort(&mut self) {}
    fn maybe_cached(&self, _: &F) -> bool {
        false
    }
}

#[derive(Debug, Default)]
pub struct CacheEq<Fragment: ToOwned> {
    previous: Option<Fragment::Owned>,
    proposition: Option<Fragment::Owned>,
}

impl<F> Driver<F> for CacheEq<F>
where
    F: Debug + Fragment + ToOwned + PartialEq<<F as ToOwned>::Owned>,
{
    type SubFragment = F;
    fn instructions<T, I>(
        &mut self,
        fragment: &F,
        transform: &mut T,
        name: &str,
    ) -> Result<Vec<CacheInstruction<T::OutputResource>>, Vec<Error>>
    where
        T: Transformation<F::Resource, I, F>,
    {
        assert!(self.proposition.is_none(), "Unclosed transaction");
        // maybe_cached means *is* cached for us
        if self.maybe_cached(fragment) {
            trace!(
                "The {} stays the same on {:?}, keeping previous",
                name,
                fragment
            );
            Ok(Vec::new())
        } else {
            trace!("New config {:?} for {}, recreating", fragment, name);
            self.proposition = Some(fragment.to_owned());
            // We just delegate to the trivial driver in such case
            // (we know it has no state at all, so we can simply create a new one).
            <TrivialDriver as Driver<F>>::instructions(
                &mut TrivialDriver,
                fragment,
                transform,
                name,
            )
        }
    }
    fn abort(&mut self) {
        assert!(
            self.proposition.take().is_some(),
            "Abort called without previous instructions"
        );
    }
    fn confirm(&mut self) {
        assert!(
            self.proposition.is_some(),
            "Confirm called without previous instructions"
        );
        self.previous = self.proposition.take();
    }
    fn maybe_cached(&self, fragment: &F) -> bool {
        // Option doesn't implement parametrized PartialEq :-(
        if let Some(prev) = self.previous.as_ref() {
            fragment == prev
        } else {
            false
        }
    }
}

#[derive(Clone, Debug, Default)]
// TODO: Use some kind of immutable/persistent data structures? Or not, this is likely to be small?
pub struct IdMapping {
    mapping: HashMap<CacheId, CacheId>,
}

impl IdMapping {
    pub fn translate<'a, R, I>(
        &'a mut self,
        id_gen: &'a mut IdGen,
        instructions: I,
    ) -> impl Iterator<Item = CacheInstruction<R>> + 'a
    where
        R: 'a,
        I: IntoIterator<Item = CacheInstruction<R>> + 'a,
    {
        instructions
            .into_iter()
            // Borrow checker notes:
            // We need to move the self and id_gen into the closure. Otherwise, it creates
            // &mut &mut IdGen monster behind the scenes, however, the outer &mut has a short
            // lifetime because it points to the function's parameter.
            //
            // The mem::swap with the HashMap below is also because of borrow checker. The drain we
            // would like to use instead would have to eat the `&mut self` (or borrow it, but see
            // the same problem above). We *know* that we won't call this again until the drain
            // iterator is wholly consumed by flat_map, but the borrow checker doesn't. So this
            // trick instead.
            .flat_map(move |i| match i {
                CacheInstruction::DropAll => {
                    let mut mapping = HashMap::new();
                    mem::swap(&mut mapping, &mut self.mapping);
                    Either::Left(
                        mapping
                            .into_iter()
                            .map(|(_, outer_id)| CacheInstruction::DropSpecific(outer_id)),
                    )
                }
                CacheInstruction::DropSpecific(id) => {
                    let id = self
                        .mapping
                        .remove(&id)
                        .expect("Inconsistent use of cache: missing ID to remove");
                    Either::Right(iter::once(CacheInstruction::DropSpecific(id)))
                }
                CacheInstruction::Install { id, resource } => {
                    let new_id = id_gen.next().expect("Run out of cache IDs? Impossible");
                    assert!(
                        self.mapping.insert(id, new_id).is_none(),
                        "Duplicate ID created"
                    );
                    Either::Right(iter::once(CacheInstruction::Install {
                        id: new_id,
                        resource,
                    }))
                }
            })
    }
}

#[derive(Debug, Default)]
struct ItemDriver<Driver> {
    driver: Driver,
    id_mapping: IdMapping,
    proposed_mapping: Option<IdMapping>,
    used: bool,
    new: bool,
}

#[derive(Debug)]
pub struct SeqDriver<Item, SlaveDriver> {
    id_gen: IdGen,
    sub_drivers: Vec<ItemDriver<SlaveDriver>>,
    transaction_open: bool,
    // TODO: Can we actually get rid of this?
    _item: PhantomData<Fn(&Item)>,
}

// The derived Default balks on Item: !Default, but we *don't* need that
impl<Item, SlaveDriver> Default for SeqDriver<Item, SlaveDriver> {
    fn default() -> Self {
        Self {
            id_gen: IdGen::new(),
            sub_drivers: Vec::new(),
            transaction_open: false,
            _item: PhantomData,
        }
    }
}

// TODO: This one is complex enough, this calls for bunch of trace and debug logging!
impl<F, I, SlaveDriver> Driver<F> for SeqDriver<I, SlaveDriver>
where
    F: Fragment,
    I: Fragment,
    for<'a> &'a F: IntoIterator<Item = &'a I>,
    SlaveDriver: Driver<I> + Default,
{
    type SubFragment = SlaveDriver::SubFragment;
    fn instructions<T, Ins>(
        &mut self,
        fragment: &F,
        transform: &mut T,
        name: &str,
    ) -> Result<Vec<CacheInstruction<T::OutputResource>>, Vec<Error>>
    where
        T: Transformation<<Self::SubFragment as Fragment>::Resource, Ins, Self::SubFragment>,
    {
        assert!(!self.transaction_open);
        self.transaction_open = true;
        let mut instructions = Vec::new();
        let mut errors = Vec::new();

        for sub in fragment {
            let existing = self
                .sub_drivers
                .iter_mut()
                .find(|d| !d.used && d.driver.maybe_cached(sub));
            // unwrap_or_else angers the borrow checker here
            let slot = if let Some(existing) = existing {
                existing
            } else {
                self.sub_drivers.push(ItemDriver::default());
                let slot = self.sub_drivers.last_mut().unwrap();
                slot.new = true;
                slot
            };

            slot.used = true;
            match slot.driver.instructions(sub, transform, name) {
                Ok(new_instructions) => {
                    let mapping = if slot.new {
                        &mut slot.id_mapping
                    } else {
                        slot.proposed_mapping = Some(slot.id_mapping.clone());
                        slot.proposed_mapping.as_mut().unwrap()
                    };
                    instructions.extend(mapping.translate(&mut self.id_gen, new_instructions));
                }
                Err(errs) => errors.extend(errs),
            }
        }

        if errors.is_empty() {
            Ok(instructions)
        } else {
            self.abort();
            Err(errors)
        }
    }
    fn confirm(&mut self) {
        assert!(self.transaction_open);
        self.transaction_open = false;
        // Get rid of the unused ones
        self.sub_drivers.retain(|s| s.used);
        // Confirm all the used ones, accept proposed mappings and mark everything as old for next
        // round.
        for sub in &mut self.sub_drivers {
            sub.driver.confirm();
            if let Some(mapping) = sub.proposed_mapping.take() {
                sub.id_mapping = mapping;
            }
            sub.new = false;
        }
    }
    fn abort(&mut self) {
        assert!(self.transaction_open);
        self.transaction_open = false;
        // Get rid of the new ones completely
        self.sub_drivers.retain(|s| !s.new);
        // Abort anything we touched before
        for sub in &mut self.sub_drivers {
            if sub.used {
                sub.driver.abort();
                sub.proposed_mapping.take();
                sub.used = false;
            }
            assert!(
                sub.proposed_mapping.is_none(),
                "Proposed mapping for something not used"
            );
        }
    }
    fn maybe_cached(&self, fragment: &F) -> bool {
        fragment.into_iter().any(|s| {
            self.sub_drivers
                .iter()
                .any(|slave| slave.driver.maybe_cached(s))
        })
    }
}

pub trait Installer<Resource, O, C>: Default {
    type UninstallHandle: Send + 'static;
    fn install(&mut self, resource: Resource) -> Self::UninstallHandle;
    fn init<B: Extensible<Opts = O, Config = C>>(&mut self, builder: B) -> Result<B, Error> {
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
    fn init<B: Extensible<Opts = O, Config = C>>(&mut self, builder: B) -> Result<B, Error> {
        self.slave.init(builder)
    }
}

struct InstallCache<I, O, C, R, H> {
    installer: I,
    cache: HashMap<CacheId, H>,
    _type: PhantomData<(R, O, C)>,
}

impl<I, O, C, R> InstallCache<I, O, C, R, I::UninstallHandle>
where
    I: Installer<R, O, C>,
{
    fn new(installer: I) -> Self {
        Self {
            installer,
            cache: HashMap::new(),
            _type: PhantomData,
        }
    }
    fn interpret(&mut self, instruction: CacheInstruction<R>) {
        match instruction {
            CacheInstruction::DropAll => self.cache.clear(),
            CacheInstruction::DropSpecific(id) => assert!(self.cache.remove(&id).is_some()),
            CacheInstruction::Install { id, resource } => {
                let handle = self.installer.install(resource);
                assert!(self.cache.insert(id, handle).is_none());
            }
        }
    }
}

// Marker trait...
pub trait Stackable {}

pub trait Fragment: Sized {
    type Driver: Driver<Self> + Default;
    type Installer: Default;
    type Seed;
    type Resource;
    fn make_seed(&self, name: &str) -> Result<Self::Seed, Error>;
    fn make_resource(&self, seed: &mut Self::Seed, name: &str) -> Result<Self::Resource, Error>;
    fn create(&self, name: &str) -> Result<Self::Resource, Error> {
        let mut seed = self.make_seed(name)?;
        self.make_resource(&mut seed, name)
    }
}

impl<'a, F> Fragment for &'a F
where
    F: Fragment,
{
    type Driver = TrivialDriver; // FIXME
    type Installer = F::Installer;
    type Seed = F::Seed;
    type Resource = F::Resource;
    fn make_seed(&self, name: &str) -> Result<Self::Seed, Error> {
        F::make_seed(*self, name)
    }
    fn make_resource(&self, seed: &mut Self::Seed, name: &str) -> Result<Self::Resource, Error> {
        F::make_resource(*self, seed, name)
    }
}

// TODO: Export the macro for other containers?
// TODO: The where-* should be where-?
macro_rules! fragment_for_seq {
    ($container: ident<$base: ident $(, $extra: ident)*> $(where $($bounds: tt)+)*) => {
        impl<$base: Clone + Fragment + Stackable + 'static $(, $extra)*> Fragment
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
                type Driver = $crate::fragment::TrivialDriver;
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

pub struct CfgExtractor<F>(F);

impl<'a, O, C: 'a, F, R> Extractor<'a, O, C> for CfgExtractor<F>
where
    F: FnMut(&'a C) -> R,
    R: Fragment + 'a,
{
    type Fragment = R;
    fn extract(&mut self, _: &'a O, config: &'a C) -> R {
        (self.0)(config)
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

#[derive(Clone, Copy, Debug, Default)]
pub struct NopTransformation;

impl<R: 'static, I, S> Transformation<R, I, S> for NopTransformation {
    type OutputResource = R;
    type OutputInstaller = I;
    fn installer(&mut self, installer: I, _: &str) -> I {
        installer
    }
    fn transform(&mut self, resource: R, _: &S, _: &str) -> Result<Self::OutputResource, Error> {
        Ok(resource)
    }
}

pub struct ChainedTransformation<A, B>(A, B);

impl<A, B, R, I, S> Transformation<R, I, S> for ChainedTransformation<A, B>
where
    A: Transformation<R, I, S>,
    B: Transformation<A::OutputResource, A::OutputInstaller, S>,
{
    type OutputResource = B::OutputResource;
    type OutputInstaller = B::OutputInstaller;
    fn installer(&mut self, installer: I, name: &str) -> B::OutputInstaller {
        let installer = self.0.installer(installer, name);
        self.1.installer(installer, name)
    }
    fn transform(
        &mut self,
        resource: R,
        fragment: &S,
        name: &str,
    ) -> Result<Self::OutputResource, Error> {
        let resource = self.0.transform(resource, fragment, name)?;
        self.1.transform(resource, fragment, name)
    }
}

pub struct SetInstaller<T, I>(T, Option<I>);

impl<T, I, R, OI, S> Transformation<R, OI, S> for SetInstaller<T, I>
where
    T: Transformation<R, OI, S>,
{
    type OutputResource = T::OutputResource;
    type OutputInstaller = I;
    fn installer(&mut self, _installer: OI, _: &str) -> I {
        self.1
            .take()
            .expect("SetInstaller::installer called more than once")
    }
    fn transform(
        &mut self,
        resource: R,
        fragment: &S,
        name: &str,
    ) -> Result<Self::OutputResource, Error> {
        self.0.transform(resource, fragment, name)
    }
}

pub struct Pipeline<Fragment, Extractor, Driver, Transformation, SpiritType> {
    name: &'static str,
    _fragment: PhantomData<Fn(Fragment)>,
    _spirit: PhantomData<Fn(SpiritType)>,
    extractor: Extractor,
    driver: Driver,
    transformation: Transformation,
}

impl Pipeline<(), (), (), (), ()> {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            _fragment: PhantomData,
            _spirit: PhantomData,
            extractor: (),
            driver: (),
            transformation: (),
        }
    }

    pub fn extract<O, C, E: for<'e> Extractor<'e, O, C>>(
        self,
        e: E,
    ) -> Pipeline<
        <E as Extractor<'static, O, C>>::Fragment,
        E,
        <<E as Extractor<'static, O, C>>::Fragment as Fragment>::Driver,
        NopTransformation,
        (O, C),
    > {
        Pipeline {
            name: self.name,
            _fragment: PhantomData,
            _spirit: PhantomData,
            extractor: e,
            driver: Default::default(),
            transformation: NopTransformation,
        }
    }

    pub fn extract_cfg<O, C: 'static, R, E>(
        self,
        e: E,
    ) -> Pipeline<R, CfgExtractor<E>, R::Driver, NopTransformation, (O, C)>
    where
        CfgExtractor<E>: for<'a> Extractor<'a, O, C>,
        E: FnMut(&'static C) -> R,
        R: Fragment,
    {
        Pipeline {
            name: self.name,
            _fragment: PhantomData,
            _spirit: PhantomData,
            extractor: CfgExtractor(e),
            driver: Default::default(),
            transformation: NopTransformation,
        }
    }
}

impl<F, E, D, T, O, C> Pipeline<F, E, D, T, (O, C)>
where
    F: Fragment,
{
    pub fn set_driver<ND: Driver<F>>(self, driver: ND) -> Pipeline<F, E, ND, T, (O, C)>
    where
        T: Transformation<<ND::SubFragment as Fragment>::Resource, F::Installer, ND::SubFragment>,
    {
        Pipeline {
            driver,
            name: self.name,
            _fragment: PhantomData,
            _spirit: PhantomData,
            extractor: self.extractor,
            transformation: self.transformation,
        }
    }
}

impl<F, E, D, T, O, C> Pipeline<F, E, D, T, (O, C)>
where
    F: Fragment,
    D: Driver<F>,
    T: Transformation<<D::SubFragment as Fragment>::Resource, F::Installer, D::SubFragment>,
{
    pub fn transform<NT>(
        self,
        transform: NT,
    ) -> Pipeline<F, E, D, ChainedTransformation<T, NT>, (O, C)>
    where
        NT: Transformation<T::OutputResource, T::OutputInstaller, D::SubFragment>,
    {
        Pipeline {
            name: self.name,
            _fragment: PhantomData,
            _spirit: PhantomData,
            driver: self.driver,
            extractor: self.extractor,
            transformation: ChainedTransformation(self.transformation, transform),
        }
    }

    pub fn install<I>(self, installer: I) -> Pipeline<F, E, D, SetInstaller<T, I>, (O, C)>
    where
        I: Installer<T::OutputResource, O, C>,
    {
        Pipeline {
            name: self.name,
            _fragment: PhantomData,
            _spirit: PhantomData,
            driver: self.driver,
            extractor: self.extractor,
            transformation: SetInstaller(self.transformation, Some(installer)),
        }
    }

    // TODO: add_installer
}

pub struct CompiledPipeline<O, C, T, I, D, E, R, H> {
    name: &'static str,
    transformation: T,
    install_cache: Arc<Mutex<InstallCache<I, O, C, R, H>>>,
    driver: Arc<Mutex<D>>,
    extractor: E,
}

pub trait BoundedCompiledPipeline<'a, O, C> {
    fn run(&mut self, opts: &'a O, config: &'a C) -> ValidationResults;
}

impl<'a, O, C, T, I, D, E> BoundedCompiledPipeline<'a, O, C>
    for CompiledPipeline<O, C, T, I, D, E, T::OutputResource, I::UninstallHandle>
where
    O: 'static,
    C: 'static,
    E: Extractor<'a, O, C>,
    D: Driver<E::Fragment> + Send + 'static,
    T: Transformation<
        <D::SubFragment as Fragment>::Resource,
        <D::SubFragment as Fragment>::Installer,
        D::SubFragment,
    >,
    T::OutputResource: 'static,
    I: Installer<T::OutputResource, O, C> + Send + 'static,
{
    fn run(&mut self, opts: &'a O, config: &'a C) -> ValidationResults {
        let fragment = self.extractor.extract(opts, config);
        let instructions =
            match self
                .driver
                .lock()
                .instructions(&fragment, &mut self.transformation, self.name)
            {
                Ok(i) => i,
                Err(errs) => return errs.into(),
            };
        let driver_f = Arc::clone(&self.driver);
        let failure = move || {
            driver_f.lock().abort();
        };
        let driver_s = Arc::clone(&self.driver);
        let install_cache = Arc::clone(&self.install_cache);
        let success = move || {
            driver_s.lock().confirm();
            let mut install_cache = install_cache.lock();
            for ins in instructions {
                install_cache.interpret(ins);
            }
        };
        ValidationResult::nothing()
            .on_abort(failure)
            .on_success(success)
            .into()
    }
}

impl<F, B, E, D, T> Extension<B> for Pipeline<F, E, D, T, (B::Opts, B::Config)>
where
    B::Config: Send + 'static,
    B::Opts: Send + 'static,
    B: Extensible<Ok = B>,
    CompiledPipeline<
        B::Opts,
        B::Config,
        T,
        T::OutputInstaller,
        D,
        E,
        T::OutputResource,
        <T::OutputInstaller as Installer<T::OutputResource, B::Opts, B::Config>>::UninstallHandle,
    >: for<'a> BoundedCompiledPipeline<'a, B::Opts, B::Config> + Send + 'static,
    D: Driver<F> + Send + 'static,
    F: Fragment,
    T: Transformation<
        <D::SubFragment as Fragment>::Resource,
        <D::SubFragment as Fragment>::Installer,
        D::SubFragment,
    >,
    T::OutputInstaller: Installer<T::OutputResource, B::Opts, B::Config> + 'static,
{
    // TODO: Extract parts & make it possible to run independently?
    // TODO: There seems to be a lot of mutexes that are not really necessary here.
    // TODO: This would use some tests
    fn apply(self, builder: B) -> Result<B, Error> {
        let mut transformation = self.transformation;
        let mut installer = transformation.installer(Default::default(), self.name);
        let builder = installer.init(builder)?;
        let mut compiled = CompiledPipeline {
            name: self.name,
            driver: Arc::new(Mutex::new(self.driver)),
            extractor: self.extractor,
            install_cache: Arc::new(Mutex::new(InstallCache::new(installer))),
            transformation,
        };
        let validator = move |_old: &_, cfg: &mut B::Config, opts: &B::Opts| -> ValidationResults {
            compiled.run(opts, cfg)
        };
        builder.config_validator(validator)
    }
}
