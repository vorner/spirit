//! A collection of [`Driver`]s for use by [`Fragment`]s.
//!
//! This module contains the [`Driver`] trait, several implementations of that trait for general
//! use and some support types around them.
//!
//! [`Driver`]: crate::fragment::driver::Driver

use std::collections::HashMap;
use std::fmt::Debug;
use std::iter;
use std::marker::PhantomData;
use std::mem;

use either::Either;
use failure::Error;
use log::{trace, warn};

use super::{Fragment, Transformation};

// XXX: Logging and tests

/// Generator of IDs for the [`Driver`].
///
/// The [`Driver`] needs to identify the resources it created, so it can later on request their
/// removal. The [`CacheId`] is used for that. That is an opaque type, so the [`Driver`] can either
/// create a default (non-unique) one or use this generator to create unique IDs.
///
/// Note that a generator will never produce two equal IDs, but two different generators can both
/// produce the same one. The [`Driver`] must take care not to use IDs from two different
/// generators.
///
/// The IDs are produced using the [`Iterator`] trait.
///
/// The generator implements a very small set of traits. In particular, it isn't [`Clone`]. This is
/// on purpose, because cloning the generators could lead to surprising behaviours.
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

/// ID used by the [`Driver`] to identify specific instance of the resource.
///
/// Only one ID can be created directly ([`CacheId::dummy`]). It can be used if the driver never
/// creates more than one instance of the resource at a time.
///
/// If unique IDs are needed, the [`Driver`] needs to keep and use an [`IdGen`] to generate them.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct CacheId(u128);

impl CacheId {
    fn dummy() -> Self {
        CacheId(0)
    }
}

/// One instruction from the [`Driver`].
///
/// The [`Driver`] issues instructions to the rest of the [`Pipeline`][super::pipeline::Pipeline].
/// They either ask it to install a new resource or to remove some previous resources.
pub enum Instruction<Resource> {
    /// Instruction for the [`Pipeline`][super::pipeline::Pipeline] to remove all active resources
    /// produced by this driver.
    ///
    /// This is legal to use even in cases when the set of active resources is empty (this drops
    /// nothing) or if the resources are not uniquely separately identifiable.
    DropAll,

    /// Instruction for the [`Pipeline`] to remove a specific resource.
    ///
    /// It is a contract violation if this ID doesn't correspond to exactly one active resource and
    /// the [`Pipeline`] may panic or misbehave under such circumstances. Specifically, it is not
    /// allowed to refer to resource that is no longer active (was dropped previously), never
    /// existed or to use a non-unique ID here.
    ///
    /// [`Pipeline`]: super::pipeline::Pipeline
    DropSpecific(CacheId),

    /// Installs another resource.
    ///
    /// The resource is identified with an `id`, so it can be referenced in the future. It *is*
    /// allowed to reuse a no longer used ID. It is *not* allowed to produce non-unique IDs for
    /// active resources.
    Install {
        /// The ID this resource will be identified as.
        id: CacheId,

        /// The resource to install.
        resource: Resource,
    },
}

impl<Resource> Instruction<Resource> {
    fn replace(resource: Resource) -> Vec<Self> {
        vec![
            Instruction::DropAll,
            Instruction::Install {
                id: CacheId::dummy(),
                resource,
            },
        ]
    }
}

/// The [`Driver`] of a [`Pipeline`].
///
/// The driver is the part of the [`Pipeline`] that decides when a cached/old version of a resource
/// is good enough to keep or when a new one needs to be created and the old one removed, and if
/// it should be recreated from scratch or the seed can be reused. It also can split the
/// [`Fragment`] into smaller [`Fragment`]s and drive the resources separately.
///
/// Any kind of caching (of either the old [`Fragment`], the [`Seed`][Fragment::Seed], the
/// [`Resource`][Fragment::Resource] or of anything else) is fully in the control of the [`Driver`]
/// itself.
///
/// Each time the configuration is reloaded, the [`Driver`] is fed with the new version of the
/// fragment. The driver can either return error (or multiple ones) or [`Instruction`]s how to
/// transition from the old state to the new one.
///
/// The [`Pipeline`] then either follows these [`Instruction`]s and calls [`confirm`], or drops the
/// instructions (for example if some other [`Pipeline`] failed) and calls [`abort`]. Exactly one
/// of these will be called after call of [`instructions`][Driver::instructions].
///
/// If [`abort`] is called, the [`Driver`] shall act in the same way as if the last
/// [`instructions`] has never been called ‒ eg. it should return to the *old* state and keep
/// caching the old data. If [`confirm`] is called, it can discard the old state and keep just the
/// new one.
///
/// The `F` generic parameter is the [`Fragment`] this driver will be provided. In contrast, the
/// [`SubFragment`] is the smaller fragment into which the [`Driver`] cuts the
/// original `F`. It may or may not be the same type.
///
/// If it cuts `F` into smaller ones, one resource for each [`SubFragment`] is to be created and
/// transformed.
///
/// The whole anatomy of how [`Pipeline`]s and [`Driver`]s work is described in the
/// [`fragment`][super] module.
///
/// [`Pipeline`]: super::pipeline::Pipeline
/// [`SubFragment`]: Driver::SubFragment
/// [`instructions`]: Driver::instructions
/// [`confirm`]: Driver::confirm
/// [`abort`]: Driver::abort
pub trait Driver<F: Fragment> {
    /// The smaller [`Fragment`] the driver cuts `F` into.
    ///
    /// This may be the same as `F` or it may be something smaller (eg. it may be the elements of
    /// `Vec<T>`.
    type SubFragment: Fragment;

    /// Issues the instructions how to transition to the new fragment.
    ///
    /// The driver needs to issue some valid sequence of instructions (see the [`Instruction`] for
    /// details what is valid) to make sure the [`Resource`] for the new [`Fragment`] is active.
    ///
    /// Note that the instructions are not automatically followed. The call of [`instructions`]
    /// will be followed by either a call to [`confirm`] or [`abort`], depending on if the whole
    /// new configuration is accepted or not. The [`Driver`] needs to take this into account when
    /// caching.
    ///
    /// Part of creation of the [`Resource`] is applying the [`Transformation`].
    ///
    /// In case there are no changes needed, empty sequence of instructions may be legally
    /// returned. The [`abort`] or [`confirm`] is called even in such case.
    ///
    /// If it is not possible to create the resource or resources (the driver *is* allowed to
    /// produce multiple), an error or errors shall be returned instead. In such case the internal
    /// cache must stay the same.
    ///
    /// [`Resource`]: Fragment::Resource
    /// [`instructions`]: Driver::instructions
    /// [`confirm`]: Driver::confirm
    /// [`abort`]: Driver::abort
    fn instructions<T, I>(
        &mut self,
        fragment: &F,
        transform: &mut T,
        name: &'static str,
    ) -> Result<Vec<Instruction<T::OutputResource>>, Vec<Error>>
    where
        T: Transformation<<Self::SubFragment as Fragment>::Resource, I, Self::SubFragment>;

    /// Call to this method informs the [`Driver`] that the instructions returned by the last call
    /// to [`instructions`] were followed and the changes have taken place.
    ///
    /// Therefore, the [`Driver`] should preserve the new state in its cache (if it does any
    /// caching).
    ///
    /// Alternatively, the [`abort`] may be called instead to inform of *not* applying the
    /// instructions.
    ///
    /// [`instructions`]: Driver::instructions
    /// [`abort`]: Driver::abort
    fn confirm(&mut self, name: &'static str);

    /// Call to this method informs the [`Driver`] that the instructions returned by the last call
    /// to [`instructions`] were *not* followed and were dropped.
    ///
    /// Therefore, the [`Driver`] should return its caches to the state before the call to
    /// [`instructions`] (if it does any caching at all).
    ///
    /// [`instructions`]: Driver::instructions
    fn abort(&mut self, name: &'static str);

    /// Informs if there's a chance the new fragment will use something in the [`Driver`]'s cache
    /// if applied.
    ///
    /// The driver shall return `true` if by using this instance of [`Driver`] (with the current
    /// state of caches) would somehow use its cache to create the resource from the provided
    /// `fragment` ‒ either by reusing it completely or by using the [`Seed`].
    ///
    /// This is used by higher-level drivers (for example the [`SeqDriver`]) to decide which slave
    /// driver should take care of which their [`SubFragment`].
    ///
    /// [`Seed`]: Fragment::Seed
    /// [`SubFragment`]: Driver::SubFragment
    fn maybe_cached(&self, frament: &F, name: &'static str) -> bool;
}

/// A trivial [`Driver`] that does no caching at all.
///
/// Every time the [`Driver`] is called, the resource is created anew and the old one replaced.
#[derive(Clone, Copy, Debug, Default)]
pub struct Trivial;

impl<F: Fragment> Driver<F> for Trivial {
    type SubFragment = F;
    fn instructions<T, I>(
        &mut self,
        fragment: &F,
        transform: &mut T,
        name: &'static str,
    ) -> Result<Vec<Instruction<T::OutputResource>>, Vec<Error>>
    where
        T: Transformation<F::Resource, I, F>,
    {
        trace!(
            "Creating resource {}, generating a replace instruction for any possible previous",
            name,
        );
        let resource = fragment
            .create(name)
            .and_then(|r| transform.transform(r, fragment, name))
            .map_err(|e| vec![e])?;
        Ok(Instruction::replace(resource))
    }
    fn confirm(&mut self, _name: &'static str) {}
    fn abort(&mut self, _name: &'static str) {}
    fn maybe_cached(&self, _: &F, _name: &'static str) -> bool {
        false
    }
}

/// A result of the [`Comparable`] trait.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Comparison {
    /// The compared [`Fragment`]s don't look similar at all.
    ///
    /// There's no chance of taking advantage of caching when creating the new [`Resource`] ‒ it
    /// would have to be created from scratch (eg. going through both stages of creation).
    ///
    /// [`Resource`]: Fragment::Resource
    Dissimilar,

    /// The compared [`Fragment`]s are somewhat similar, but not the same.
    ///
    /// It will be possible to use the same [`Seed`], but the second stage of resource creation
    /// needs to be done again.
    ///
    /// [`Seed`]: Fragment::Seed
    Similar,

    /// The [`Fragment`]s are exactly the same.
    ///
    /// The resource doesn't have to be changed, the old instance is good.
    Same,
}

/// [`Fragment`]s that can be compared for similarity.
///
/// This is used by the [`CacheSimilar`] [`Driver`].
pub trait Comparable<RHS = Self> {
    /// Compares two fragments.
    fn compare(&self, other: &RHS) -> Comparison;
}

#[derive(Debug)]
enum Proposition<F: Fragment + ToOwned> {
    Nothing,
    ReplaceFragment(F::Owned),
    ReplaceBoth { fragment: F::Owned, seed: F::Seed },
}

impl<F: Fragment + ToOwned> Proposition<F> {
    fn active(&self) -> bool {
        match self {
            Proposition::Nothing => false,
            _ => true,
        }
    }
}

/// A [`Driver`] that caches both created [`Resource`]s and their [`Seed`]s to create as little as
/// possible.
///
/// It uses the [`Comparable`] trait to decide if it needs to create the [`Resource`] complete from
/// scratch, if it can reuse the old [`Seed`] or if it can even keep the old instance. This is a
/// good driver for cases where the [`Seed`] is some kind of unique resource and changing the
/// resource by creating a completely new instance while the old one existed wouldn't work. An
/// example of this is the bound ports on listening sockets ‒ while we might want to attach
/// different configuration to the same socket, opening a new socket with the same port would not
/// work. Therefore, the socket is somewhere within the [`Seed`] and it is reused if the port
/// doesn't change.
///
/// [`Resource`]: Fragment::Resource
/// [`Seed`]: Fragment::Seed
#[derive(Debug)]
pub struct CacheSimilar<F: Fragment + ToOwned> {
    previous: Option<F::Owned>,
    seed: Option<F::Seed>,
    proposition: Proposition<F>,
}

impl<F: Fragment + ToOwned> Default for CacheSimilar<F> {
    fn default() -> Self {
        CacheSimilar {
            previous: None,
            seed: None,
            proposition: Proposition::Nothing,
        }
    }
}

impl<F> CacheSimilar<F>
where
    F: Debug + Fragment + ToOwned + Comparable<<F as ToOwned>::Owned>,
{
    fn compare(&self, fragment: &F) -> Comparison {
        self.previous
            .as_ref()
            .map(|prev| fragment.compare(prev))
            .unwrap_or(Comparison::Dissimilar)
    }
}

impl<F> Driver<F> for CacheSimilar<F>
where
    F: Debug + Fragment + ToOwned + Comparable<<F as ToOwned>::Owned>,
{
    type SubFragment = F;
    fn instructions<T, I>(
        &mut self,
        fragment: &F,
        transform: &mut T,
        name: &'static str,
    ) -> Result<Vec<Instruction<T::OutputResource>>, Vec<Error>>
    where
        T: Transformation<F::Resource, I, F>,
    {
        assert!(!self.proposition.active(), "Unclosed transaction");

        match self.compare(fragment) {
            Comparison::Dissimilar => {
                trace!(
                    "Completely new config {:?} for {}, recreating from scratch",
                    fragment,
                    name
                );
                let mut new_seed = fragment.make_seed(name).map_err(|e| vec![e])?;
                let resource = fragment
                    .make_resource(&mut new_seed, name)
                    .and_then(|r| transform.transform(r, fragment, name))
                    .map_err(|e| vec![e])?;
                self.proposition = Proposition::ReplaceBoth {
                    seed: new_seed,
                    fragment: fragment.to_owned(),
                };

                Ok(Instruction::replace(resource))
            }
            Comparison::Similar => {
                trace!(
                    "A similar config {:?} for {}, recreating from previous seed",
                    fragment,
                    name
                );
                let resource = fragment
                    .make_resource(self.seed.as_mut().expect("Missing previous seed"), name)
                    .and_then(|r| transform.transform(r, fragment, name))
                    .map_err(|e| vec![e])?;
                self.proposition = Proposition::ReplaceFragment(fragment.to_owned());

                Ok(Instruction::replace(resource))
            }
            Comparison::Same => {
                trace!(
                    "The {} stays the same on {:?}, keeping previous resource",
                    name,
                    fragment
                );
                Ok(Vec::new())
            }
        }
    }
    fn abort(&mut self, name: &'static str) {
        trace!("Aborting {}", name);
        self.proposition = Proposition::Nothing;
    }
    fn confirm(&mut self, name: &'static str) {
        trace!("Confirming {}", name);
        let mut proposition = Proposition::Nothing;
        mem::swap(&mut proposition, &mut self.proposition);
        match proposition {
            Proposition::Nothing => (),
            Proposition::ReplaceFragment(f) => self.previous = Some(f),
            Proposition::ReplaceBoth { fragment, seed } => {
                self.seed = Some(seed);
                self.previous = Some(fragment);
            }
        }
    }
    fn maybe_cached(&self, fragment: &F, _name: &'static str) -> bool {
        self.compare(fragment) != Comparison::Dissimilar
    }
}

/// A [`Driver`] that caches the [`Resource`] if the [`Fragment`] doesn't change at all.
///
/// Most of the time, the configuration is the same or almost the same ‒ so many [`Resource`]s
/// don't need to be changed. This driver keeps the old instance of the [`Fragment`] and if the new
/// one compares equal, it does nothing (eg. keeps the old instance of the [`Resource`]).
///
/// [`Resource`]: Fragment::Resource
#[derive(Debug)]
pub struct CacheEq<Fragment: ToOwned> {
    previous: Option<Fragment::Owned>,
    proposition: Option<Fragment::Owned>,
}

impl<F: ToOwned> Default for CacheEq<F> {
    fn default() -> Self {
        CacheEq {
            previous: None,
            proposition: None,
        }
    }
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
        name: &'static str,
    ) -> Result<Vec<Instruction<T::OutputResource>>, Vec<Error>>
    where
        T: Transformation<F::Resource, I, F>,
    {
        assert!(self.proposition.is_none(), "Unclosed transaction");
        // maybe_cached means *is* cached for us
        if self.maybe_cached(fragment, name) {
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
            <Trivial as Driver<F>>::instructions(&mut Trivial, fragment, transform, name)
        }
    }
    fn abort(&mut self, name: &'static str) {
        trace!("Aborting {}", name);
        // Note: we don't check if anything is in here, because in case these were the same we
        // didn't fill in the proposition.
    }
    fn confirm(&mut self, name: &'static str) {
        trace!("Confirming {}", name);
        if let Some(proposition) = self.proposition.take() {
            self.previous = Some(proposition);
        }
    }
    fn maybe_cached(&self, fragment: &F, _name: &'static str) -> bool {
        // Option doesn't implement parametrized PartialEq :-(
        if let Some(prev) = self.previous.as_ref() {
            fragment == prev
        } else {
            false
        }
    }
}

/// An object to map [`Instruction`]s from multiple drivers into instruction sequences not
/// containing duplicate IDs.
///
/// If one [`Driver`] uses services of some other slave drivers, it is up to the master to make
/// sure instructions gathered from the slaves don't collide on IDs. This helps in that regard.
///
/// It is expected to be used as:
/// * One mapping per slave driver, but sharing the [`IdGen`].
/// * All instructions from the slave are [`translate`]d through the mapping.
/// * The mapping is only stored and cached if [`confirm`] is called.
///
/// TODO Example
///
/// [`translate`]: IdMapping::translate
/// [`confirm`]: Driver::confirm
#[derive(Clone, Debug, Default)]
pub struct IdMapping {
    mapping: HashMap<CacheId, CacheId>,
}

impl IdMapping {
    /// Assigns new IDs so they are unique within the `IdMapping`s sharing the same `id_gen`.
    ///
    /// This changes the IDs so they are unique. It also changes the IDs used in the drop
    /// instructions to reflect the original changes and [`DropAll`][Instruction::DropAll] is
    /// expanded to separate instructions (because in the bigger context of the master, all might
    /// mean resources from other slave drivers).
    pub fn translate<'a, R, I>(
        &'a mut self,
        id_gen: &'a mut IdGen,
        instructions: I,
    ) -> impl Iterator<Item = Instruction<R>> + 'a
    where
        R: 'a,
        I: IntoIterator<Item = Instruction<R>> + 'a,
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
                Instruction::DropAll => {
                    let mut mapping = HashMap::new();
                    mem::swap(&mut mapping, &mut self.mapping);
                    Either::Left(
                        mapping
                            .into_iter()
                            .map(|(_, outer_id)| Instruction::DropSpecific(outer_id)),
                    )
                }
                Instruction::DropSpecific(id) => {
                    let id = self
                        .mapping
                        .remove(&id)
                        .expect("Inconsistent use of cache: missing ID to remove");
                    Either::Right(iter::once(Instruction::DropSpecific(id)))
                }
                Instruction::Install { id, resource } => {
                    let new_id = id_gen.next().expect("Run out of cache IDs? Impossible");
                    assert!(
                        self.mapping.insert(id, new_id).is_none(),
                        "Duplicate ID created"
                    );
                    Either::Right(iter::once(Instruction::Install {
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

/// A plumbing [`Driver`] for sequences of fragments.
///
/// This driver is used to go from single [`Fragment`] to a sequence ‒ this is driver for things
/// like `Vec<F>` or `HashSet<F>`.
#[derive(Debug)]
pub struct SeqDriver<Item, SlaveDriver> {
    id_gen: IdGen,
    sub_drivers: Vec<ItemDriver<SlaveDriver>>,
    transaction_open: bool,
    // TODO: Can we actually get rid of this?
    _item: PhantomData<fn(&Item)>,
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

// TODO: This might use some nice tests.
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
        name: &'static str,
    ) -> Result<Vec<Instruction<T::OutputResource>>, Vec<Error>>
    where
        T: Transformation<<Self::SubFragment as Fragment>::Resource, Ins, Self::SubFragment>,
    {
        assert!(!self.transaction_open);
        trace!("Updating sequence {}", name);
        self.transaction_open = true;
        let mut instructions = Vec::new();
        let mut errors = Vec::new();

        for sub in fragment {
            let existing = self
                .sub_drivers
                .iter_mut()
                .find(|d| !d.used && d.driver.maybe_cached(sub, name));
            // unwrap_or_else angers the borrow checker here
            let slot = if let Some(existing) = existing {
                trace!("Found existing version of instance in {}", name);
                existing
            } else {
                trace!(
                    "Previous version of instance in {} not found, creating a new one",
                    name
                );
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
            self.abort(name);
            Err(errors)
        }
    }
    fn confirm(&mut self, name: &'static str) {
        trace!("Confirming the whole sequence {}", name);
        assert!(self.transaction_open);
        self.transaction_open = false;
        // Get rid of the unused ones
        self.sub_drivers.retain(|s| s.used);
        // Confirm all the used ones, accept proposed mappings and mark everything as old for next
        // round.
        for sub in &mut self.sub_drivers {
            sub.driver.confirm(name);
            if let Some(mapping) = sub.proposed_mapping.take() {
                sub.id_mapping = mapping;
            }
            sub.new = false;
            sub.used = false;
        }
    }
    fn abort(&mut self, name: &'static str) {
        trace!("Aborting the whole sequence of {}", name);
        assert!(self.transaction_open);
        self.transaction_open = false;
        // Get rid of the new ones completely
        self.sub_drivers.retain(|s| !s.new);
        // Abort anything we touched before
        for sub in &mut self.sub_drivers {
            if sub.used {
                sub.driver.abort(name);
                sub.proposed_mapping.take();
                sub.used = false;
            }
            assert!(
                sub.proposed_mapping.is_none(),
                "Proposed mapping for something not used"
            );
        }
    }
    fn maybe_cached(&self, fragment: &F, name: &'static str) -> bool {
        fragment.into_iter().any(|s| {
            self.sub_drivers
                .iter()
                .any(|slave| slave.driver.maybe_cached(s, name))
        })
    }
}

/// A [`Driver`] for a single-shot initialization.
///
/// This driver creates the resource only the first time it is called. On an attempt to call it
/// again it warns if the value of the fragment is different.
struct OnceDriver<F: ToOwned> {
    loaded: Option<F::Owned>,
    initial: bool,
}

impl<F> Default for OnceDriver<F>
where
    F: ToOwned,
{
    fn default() -> Self {
        OnceDriver {
            loaded: None,
            initial: true,
        }
    }
}

impl<F> Driver<F> for OnceDriver<F>
where
    F: Fragment + PartialEq<<F as ToOwned>::Owned> + ToOwned + 'static,
{
    type SubFragment = F;
    fn instructions<T, I>(
        &mut self,
        fragment: &F,
        transform: &mut T,
        name: &'static str,
    ) -> Result<Vec<Instruction<T::OutputResource>>, Vec<Error>>
    where
        T: Transformation<<Self::SubFragment as Fragment>::Resource, I, Self::SubFragment>,
    {
        if let Some(loaded) = self.loaded.as_ref() {
            if fragment == loaded {
                warn!(
                    "{} changed in configuration, can't change at runtime, keeping previous",
                    name,
                );
            }
            Ok(Vec::new())
        } else {
            assert!(self.initial);
            trace!("Building {} for the first time", name);
            self.loaded = Some(fragment.to_owned());
            Trivial.instructions(fragment, transform, name)
        }
    }
    fn confirm(&mut self, _name: &'static str) {
        assert!(self.loaded.is_some(), "Confirm called before instructions");
        self.initial = false;
    }
    fn abort(&mut self, _name: &'static str) {
        if self.initial {
            // Keep initial to true in such case
            assert!(
                self.loaded.take().is_some(),
                "Abort called before instructions"
            );
        }
        // else - we still keep the thing that was set, because this must have been from previous
        // round
    }
    fn maybe_cached(&self, fragment: &F, _name: &'static str) -> bool {
        self.loaded
            .as_ref()
            .map(|l| fragment == l)
            .unwrap_or_default()
    }
}

/// An adaptor [`Driver`] for references.
///
/// This is used behind the scenes to wrap a driver for `F` to create a driver for `&F`.
#[derive(Debug, Default)]
pub struct RefDriver<Inner>(Inner);

impl<Inner> RefDriver<Inner> {
    /// Creates the driver.
    ///
    /// It is available to support also drivers that are provided and created by the user. Usually,
    /// the `Default` implementation is used within the [`Fragment`].
    pub fn new(inner: Inner) -> Self {
        RefDriver(inner)
    }
}

impl<'a, F: Fragment, Inner: Driver<F>> Driver<&'a F> for RefDriver<Inner> {
    type SubFragment = Inner::SubFragment;
    fn instructions<T, I>(
        &mut self,
        fragment: &&F,
        transform: &mut T,
        name: &'static str,
    ) -> Result<Vec<Instruction<T::OutputResource>>, Vec<Error>>
    where
        T: Transformation<<Self::SubFragment as Fragment>::Resource, I, Self::SubFragment>,
    {
        self.0.instructions(*fragment, transform, name)
    }
    fn confirm(&mut self, name: &'static str) {
        self.0.confirm(name);
    }
    fn abort(&mut self, name: &'static str) {
        self.0.abort(name);
    }
    fn maybe_cached(&self, fragment: &&F, name: &'static str) -> bool {
        self.0.maybe_cached(*fragment, name)
    }
}
