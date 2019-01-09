use std::collections::HashMap;
use std::fmt::Debug;
use std::iter;
use std::marker::PhantomData;
use std::mem;

use either::Either;
use failure::Error;
use log::trace;

use super::{Fragment, Transformation};

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

#[derive(Debug, Default)]
pub struct RefDriver<Inner>(Inner);

impl<Inner> RefDriver<Inner> {
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
        name: &str,
    ) -> Result<Vec<CacheInstruction<T::OutputResource>>, Vec<Error>>
    where
        T: Transformation<<Self::SubFragment as Fragment>::Resource, I, Self::SubFragment>
    {
        self.0.instructions(*fragment, transform, name)
    }
    fn confirm(&mut self) {
        self.0.confirm();
    }
    fn abort(&mut self) {
        self.0.abort();
    }
    fn maybe_cached(&self, fragment: &&F) -> bool {
        self.0.maybe_cached(*fragment)
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

