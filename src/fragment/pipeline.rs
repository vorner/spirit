//! The home of the [`Pipeline`].
//!
//! The high level overview of what a [`Pipeline`] is and how it works is in the
//! [`fragment`][crate::fragment] module.
//!
//! The rest of the things here is mostly support plumbing and would optimally be hidden out of the
//! public interface, but it needs to be public due to limitations of Rust. The user doesn't need
//! to care much about the other types. Despite all that, the types are not hidden from the
//! documentation to provide some guidance and clickable links.
//!
//! [`Pipeline`]: crate::fragment::pipeline::Pipeline
use std::collections::HashMap;
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::marker::PhantomData;
use std::sync::Arc;

use failure::{Backtrace, Error, Fail};
use log::{debug, trace};
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use structopt::StructOpt;

use super::driver::{CacheId, Driver, Instruction};
use super::{Extractor, Fragment, Installer, Transformation};
use crate::extension::{Extensible, Extension};
use crate::validation::Action;

/// An error caused by multiple other errors.
///
/// Carries all the errors that caused it to fail (publicly accessible). Multiple are possible.
///
/// The `cause` is delegated to the first error, if any is present.
#[derive(Debug)]
pub struct MultiError {
    /// All the errors that happened.
    pub errors: Vec<Error>,

    /// The pipeline this error comes from.
    pub pipeline: &'static str,
}

impl MultiError {
    /// Creates a multi-error.
    ///
    /// Depending on if one error is passed or multiple, the error is either propagated through
    /// (without introducing another layer of indirection) or all the errors are wrapped into a
    /// `MultiError`.
    ///
    /// # Panics
    ///
    /// If the `errs` passed is empty (eg. then there are no errors, so it logically makes no sense
    /// to call it an error).
    pub fn wrap(mut errs: Vec<Error>, pipeline: &'static str) -> Error {
        match errs.len() {
            0 => panic!("No errors in multi-error"),
            1 => errs.pop().unwrap(),
            _ => MultiError {
                errors: errs,
                pipeline,
            }
            .into(),
        }
    }
}

impl Display for MultiError {
    fn fmt(&self, formatter: &mut Formatter) -> FmtResult {
        write!(
            formatter,
            "Pipeline {} failed with {} errors",
            self.pipeline,
            self.errors.len()
        )
    }
}

impl Fail for MultiError {
    // There may actually be multiple causes. But we just stick with the first one for lack of
    // better way to pick.
    fn cause(&self) -> Option<&dyn Fail> {
        self.errors.get(0).map(Error::as_fail)
    }

    fn backtrace(&self) -> Option<&Backtrace> {
        self.errors.get(0).map(Error::backtrace)
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
    fn interpret(&mut self, instruction: Instruction<R>, name: &'static str) {
        match instruction {
            Instruction::DropAll => self.cache.clear(),
            Instruction::DropSpecific(id) => assert!(self.cache.remove(&id).is_some()),
            Instruction::Install { id, resource } => {
                let handle = self.installer.install(resource, name);
                assert!(self.cache.insert(id, handle).is_none());
            }
        }
    }
}

/// A wrapper to turn a `FnMut(Config) -> R` into an [`Extractor`].
///
/// This isn't used by the user directly, it is constructed through the
/// [`extract_cfg`][Pipeline::extract_cfg] method.
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

/// A [`Transformation`] that does nothing.
///
/// This is used at the beginning of constructing a [`Pipeline`] to plug the type parameter.
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

/// A wrapper that composes two [`Transformation`]s into one.
///
/// This applies first the transformation `A`, then `B`. It is used internally to compose things
/// together when the [`transform`][Pipeline::transform] is called.
pub struct ChainedTransformation<A, B>(A, B);

impl<A, B, R, I, S> Transformation<R, I, S> for ChainedTransformation<A, B>
where
    A: Transformation<R, I, S>,
    B: Transformation<A::OutputResource, A::OutputInstaller, S>,
{
    type OutputResource = B::OutputResource;
    type OutputInstaller = B::OutputInstaller;
    fn installer(&mut self, installer: I, name: &'static str) -> B::OutputInstaller {
        let installer = self.0.installer(installer, name);
        self.1.installer(installer, name)
    }
    fn transform(
        &mut self,
        resource: R,
        fragment: &S,
        name: &'static str,
    ) -> Result<Self::OutputResource, Error> {
        let resource = self.0.transform(resource, fragment, name)?;
        self.1.transform(resource, fragment, name)
    }
}

/// A [`Transformation`] that replaces the [`Installer`] of a pipeline.
///
/// Used internally to implement the [`install`][Pipeline::install].
pub struct SetInstaller<T, I>(T, Option<I>);

impl<T, I, R, OI, S> Transformation<R, OI, S> for SetInstaller<T, I>
where
    T: Transformation<R, OI, S>,
{
    type OutputResource = T::OutputResource;
    type OutputInstaller = I;
    fn installer(&mut self, _installer: OI, _: &'static str) -> I {
        self.1
            .take()
            .expect("SetInstaller::installer called more than once")
    }
    fn transform(
        &mut self,
        resource: R,
        fragment: &S,
        name: &'static str,
    ) -> Result<Self::OutputResource, Error> {
        self.0.transform(resource, fragment, name)
    }
}

/// A [`Transformation`] than maps a [`Resource`] through a closure.
///
/// This is used internally, to implement the [`map`][Pipeline::map] method. The user should not
/// have to come into direct contact with this.
///
/// [`Resource`]: Fragment::Resource
pub struct Map<T, M>(T, M);

impl<T, M, Rin, Rout, I, S> Transformation<Rin, I, S> for Map<T, M>
where
    T: Transformation<Rin, I, S>,
    M: FnMut(T::OutputResource) -> Rout,
    Rout: 'static,
{
    type OutputResource = Rout;
    type OutputInstaller = T::OutputInstaller;
    fn installer(&mut self, installer: I, name: &'static str) -> T::OutputInstaller {
        self.0.installer(installer, name)
    }
    fn transform(
        &mut self,
        resource: Rin,
        fragment: &S,
        name: &'static str,
    ) -> Result<Rout, Error> {
        let r = self.0.transform(resource, fragment, name)?;
        trace!("Mapping resource {}", name);
        Ok((self.1)(r))
    }
}

/// The [`Pipeline`] itself.
///
/// The high-level idea behind the [`Pipeline`] is described as part of the [`fragment`][super]
/// module documentation.
///
/// # Limitations
///
/// In a sense, the code here moves close to what is possible to do with the Rust type system. This
/// is needed to make the interface flexible ‒ the [`Pipeline`] can describe a lot of different use
/// cases on top of completely different types and [`Resource`]s.
///
/// That, however, brigs certain limitations that you might want to know about:
///
/// * All the methods and types here are very rich in terms of type parameters and trait bounds.
/// * The error messages are quite long and hard to read as a result.
/// * Sometimes, `rustc` even gives up on producing the helpful hints as a result of the above.
///   There's a workaround for that in the form of the [`check`][Pipeline::check] method.
/// * As of rust stable 1.32, extracting references (through the [`extract`][Pipeline::extract] and
///   [`extract_cfg`][Pipeline::extract_cfg]) doesn't work. Either use a newer compiler or extract
///   only owned types (eg. `clone` ‒ it should be generally cheap, because these are parts of
///   configuration the user have written and it needs to be extracted only when reloading the
///   configuration).
/// * As the pipeline is being constructed through the builder pattern, it is possible the types
///   don't line up during the construction. If you do not make it align by the end, it will not be
///   possible to use the pipeline inside the
///   [`Extensible::with`][crate::extension::Extensible::with].
///
/// In general, each [`Fragment`] comes with an example containing its *canonical* pipeline.
/// Copy-pasting and possibly modifying that is probably the easiest way to overcome the above
/// limitations.
///
/// # Creation order
///
/// The pipeline describes a mostly linear process that happens every time a configuration is
/// loaded. Therefore, it helps readability if the builder pattern of the [`Pipeline`] is written
/// in the same order as the operations happen. In addition, not all orders of the builder pattern
/// will be accepted, due to how the trait bounds are composed.
///
/// While not all steps are needed every time, the ones present should be written in this order:
///
/// * [`new`][Pipeline::new]: This is the constructor and sets the name of the pipeline.
/// * [`extract`][Pipeline::extract] or [`extract_cfg`][Pipeline::extract_cfg]: This sets the
///   closure (or other [`Extractor`]) the pipeline will use. This also sets the [`Fragment`] tied
///   to the pipeline and presets the [`Driver`] and the [`Installer`] (though the [`Installer`]
///   maybe something useless, since not all [`Fragment`]s create something directly installable).
/// * [`set_driver`][Pipeline::set_driver]: This overrides the default [`Driver`] provided by the
///   [`Fragment`] to something user-provided. It is generally rare to use this.
/// * [`transform`][Pipeline::transform]: This applies (another) transformation. It makes sense to
///   call multiple times to chain multiple transformations together. They are applied in the same
///   order as they are added.
/// * [`install`][Pipeline::install]: Sets or overrides the [`Installer`] the pipeline uses. This
///   is sometimes necessary, but sometimes either the [`Fragment`] or one of the
///   [`Transformation`]s provides one.
///
/// [`Resource`]: Fragment::Resource
pub struct Pipeline<Fragment, Extractor, Driver, Transformation, SpiritType> {
    name: &'static str,
    _fragment: PhantomData<dyn Fn(Fragment)>,
    _spirit: PhantomData<dyn Fn(SpiritType)>,
    extractor: Extractor,
    driver: Driver,
    transformation: Transformation,
}

impl Pipeline<(), (), (), (), ()> {
    /// Starts creating a new pipeline.
    ///
    /// This initializes a completely useless and empty pipeline. It only sets the name, but other
    /// properties (at least the [`Extractor`]) need to be set for the [`Pipeline`] to be of any
    /// practical use.
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

    /// Sets the [`Extractor`].
    ///
    /// This ties the [`Pipeline`] to an extractor. In addition, it also sets the type of config
    /// and command line options structures, the [`Fragment`] this pipeline works with and sets the
    /// default [`Driver`] and [`Installer`] as provided by the [`Fragment`].
    ///
    /// Depending on the [`Fragment`], this might make the [`Pipeline`] usable ‒ or not, as some
    /// [`Fragment`]s can't provide reasonable (working) [`Installer`].
    ///
    /// As of `rustc` 1.32, it is not possible to return references (or types containing them) into
    /// the config or command line structures (it is unable to prove some of the trait bounds). You
    /// can either return an owned version of the type (eg. with `.clone()`) or use a newer version
    /// of the compiler.
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
        trace!("Configured extractor on pipeline {}", self.name);
        Pipeline {
            name: self.name,
            _fragment: PhantomData,
            _spirit: PhantomData,
            extractor: e,
            driver: Default::default(),
            transformation: NopTransformation,
        }
    }

    /// Sets the [`Extractor`] to a closure taking only the configuration.
    ///
    /// This is a convenience wrapper around [`extract`][Pipeline::extract]. It acts the same way,
    /// only the closure has just one parameter ‒ the configuration. Most of the extracted
    /// configuration fragments come from configuration anyway.
    pub fn extract_cfg<O, C: 'static, R, E>(
        self,
        e: E,
    ) -> Pipeline<R, CfgExtractor<E>, R::Driver, NopTransformation, (O, C)>
    where
        CfgExtractor<E>: for<'a> Extractor<'a, O, C>,
        E: FnMut(&'static C) -> R,
        R: Fragment,
    {
        trace!("Configured extractor on pipeline {}", self.name);
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
    /// Overwrites the [`Driver`] of this pipeline.
    ///
    /// Most of the time, the [`Driver`] provided by the [`Fragment`] set through the
    /// [`extract`][Pipeline::extract] method is good enough, so it is rare the user would need to
    /// call this.
    pub fn set_driver<ND: Driver<F>>(self, driver: ND) -> Pipeline<F, E, ND, T, (O, C)>
    where
        T: Transformation<<ND::SubFragment as Fragment>::Resource, F::Installer, ND::SubFragment>,
    {
        trace!("Overriding the driver on pipeline {}", self.name);
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
    /// Applies a transformation to the [`Resource`][Fragment::Resource].
    ///
    /// This puts another transformation to the end of the transformation chain.
    ///
    /// Transformations can to quite arbitrary things with the [`Resource`], including changing its
    /// type (or changing the [`Installer`] ‒ which might actually be needed when changing the
    /// type).
    ///
    /// [`Resource`]: Fragment::Resource
    pub fn transform<NT>(
        self,
        transform: NT,
    ) -> Pipeline<F, E, D, ChainedTransformation<T, NT>, (O, C)>
    where
        NT: Transformation<T::OutputResource, T::OutputInstaller, D::SubFragment>,
    {
        trace!("Adding a transformation to pipeline {}", self.name);
        Pipeline {
            name: self.name,
            _fragment: PhantomData,
            _spirit: PhantomData,
            driver: self.driver,
            extractor: self.extractor,
            transformation: ChainedTransformation(self.transformation, transform),
        }
    }

    /// Maps the [`Resource`] through a closure.
    ///
    /// This is somewhat similar to [`transform`] in that it can modify or replace the resource
    /// while it goes through the pipeline. But it is much simpler ‒ only the [`Resource`] is
    /// passed to the closure (not any configuration, names, etc). And the closure is not allowed
    /// to fail. This is mostly for convenience, so in the simple case one does not have to build
    /// the full [`Transformation`].
    ///
    /// [`transform`]: Pipeline::transform
    /// [`Resource`]: Fragment::Resource.
    pub fn map<M, R>(self, m: M) -> Pipeline<F, E, D, Map<T, M>, (O, C)>
    where
        M: FnMut(T::OutputResource) -> R,
    {
        trace!("Adding a map transformation to pipeline {}", self.name);
        Pipeline {
            name: self.name,
            _fragment: PhantomData,
            _spirit: PhantomData,
            driver: self.driver,
            extractor: self.extractor,
            transformation: Map(self.transformation, m),
        }
    }

    /// Sets the [`Installer`] used by the pipeline.
    ///
    /// The pipeline will end with the given [`Installer`] and use it to install the created
    /// [`Resource`][Fragment::Resource]s.
    pub fn install<I>(self, installer: I) -> Pipeline<F, E, D, SetInstaller<T, I>, (O, C)>
    where
        I: Installer<T::OutputResource, O, C>,
    {
        trace!("Setting installer to pipeline {}", self.name);
        Pipeline {
            name: self.name,
            _fragment: PhantomData,
            _spirit: PhantomData,
            driver: self.driver,
            extractor: self.extractor,
            transformation: SetInstaller(self.transformation, Some(installer)),
        }
    }

    /// A workaround for missing trait hints in error messages.
    ///
    /// Sometimes, `rustc` gives up on the complexity of the trait bounds and simply says the
    /// [`Extension`] trait is not implemented ‒ but one would need a lot of guessing to know *why*
    /// it is not implemented.
    ///
    /// The `check` method doesn't change the pipeline in any way, but it has a *subset* of the
    /// trait bounds on it. Usually, the missing or broken part is detected by these trait bounds,
    /// but they are also significantly simpler than the full set, so `rustc` is willing to issue
    /// the hints.
    pub fn check(self) -> Self
    where
        D::SubFragment: Fragment,
        T::OutputInstaller: Installer<T::OutputResource, O, C>,
    {
        self
    }

    // TODO: add_installer
}

/// An internal intermediate type.
///
/// This is used internally to represent the [`Pipeline`] after it has been inserted into the
/// [`Extensible`][crate::Extensible].
///
/// While it is quite useless (and impossible to get hands on), it will probably be possible to
/// construct one explicitly and use to run the pipeline in a manual way one day.
pub struct CompiledPipeline<O, C, T, I, D, E, R, H> {
    name: &'static str,
    transformation: T,
    install_cache: InstallCache<I, O, C, R, H>,
    driver: D,
    extractor: E,
}

impl<O, C, T, I, D, E, R, H> CompiledPipeline<O, C, T, I, D, E, R, H> {
    // :-| Borrow checker is not that smart to be able to pass two mutable sub-borrows through the
    // deref trait. So this one allows us to smuggle it through the one on `self` and get the two
    // on the other side.
    fn explode(&mut self) -> (&'static str, &mut T, &mut D) {
        (self.name, &mut self.transformation, &mut self.driver)
    }
}

/// Trait alias for one concrete lifetime of a [`Pipeline`].
///
/// Pipelines are fed with only references to the configuration and command line options and a lot
/// of the processing can happen through references only. As a result, most of the trait bounds in
/// around the pipelines are [HRTBs](https://doc.rust-lang.org/nomicon/hrtb.html).
///
/// This is an internal trait alias, describing the [`Pipeline`] bounds for a single concrete
/// lifetime. This makes the bounds of the [`Extension`] implementation actually almost manageable
/// instead of completely crackpot insane.
///
/// However, as the user is not able to get the hands on any instance implementing this trait, it
/// is quite useless and is public only through the trait bounds.
pub trait BoundedCompiledPipeline<'a, O, C> {
    /// Performs one iteration of the lifetime.
    fn run(me: &Arc<Mutex<Self>>, opts: &'a O, config: &'a C) -> Result<Action, Vec<Error>>;
}

impl<'a, O, C, T, I, D, E> BoundedCompiledPipeline<'a, O, C>
    for CompiledPipeline<O, C, T, I, D, E, T::OutputResource, I::UninstallHandle>
where
    O: 'static,
    C: 'static,
    E: Extractor<'a, O, C> + 'static,
    D: Driver<E::Fragment> + Send + 'static,
    T: Transformation<
            <D::SubFragment as Fragment>::Resource,
            <D::SubFragment as Fragment>::Installer,
            D::SubFragment,
        > + 'static,
    T::OutputResource: 'static,
    I: Installer<T::OutputResource, O, C> + Send + 'static,
{
    fn run(me: &Arc<Mutex<Self>>, opts: &'a O, config: &'a C) -> Result<Action, Vec<Error>> {
        let mut me_lock = me.lock();
        let fragment = me_lock.extractor.extract(opts, config);
        let (name, transform, driver) = me_lock.explode();
        debug!("Running pipeline {}", name);
        let instructions = driver.instructions(&fragment, transform, name)?;
        let me_f = Arc::clone(&me);
        let failure = move || {
            debug!("Rolling back pipeline {}", name);
            me_f.lock().driver.abort(name);
        };
        let me_s = Arc::clone(&me);
        let success = move || {
            debug!(
                "Success for pipeline {}, performing {} install instructions",
                name,
                instructions.len(),
            );
            let mut me = me_s.lock();
            me.driver.confirm(name);
            let name = me.name;
            for ins in instructions {
                me.install_cache.interpret(ins, name);
            }
        };
        Ok(Action::new().on_abort(failure).on_success(success))
    }
}

impl<F, B, E, D, T> Extension<B> for Pipeline<F, E, D, T, (B::Opts, B::Config)>
where
    B::Config: DeserializeOwned + Send + Sync + 'static,
    B::Opts: StructOpt + Send + Sync + 'static,
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
    fn apply(self, mut builder: B) -> Result<B, Error> {
        trace!("Inserting pipeline {}", self.name);
        let mut transformation = self.transformation;
        let mut installer = transformation.installer(Default::default(), self.name);
        builder = F::init(builder, self.name)?;
        builder = installer.init(builder, self.name)?;
        let compiled = CompiledPipeline {
            name: self.name,
            driver: self.driver,
            extractor: self.extractor,
            install_cache: InstallCache::new(installer),
            transformation,
        };
        let compiled = Arc::new(Mutex::new(compiled));
        let name = self.name;
        if F::RUN_BEFORE_CONFIG && !B::STARTED {
            let compiled = Arc::clone(&compiled);
            let before_config = move |cfg: &B::Config, opts: &B::Opts| {
                BoundedCompiledPipeline::run(&compiled, opts, cfg)
                    .map(|action| action.run(true))
                    .map_err(|errs| MultiError::wrap(errs, name))
            };
            builder = builder.before_config(before_config)?;
        }
        let validator = move |_old: &_, cfg: &Arc<B::Config>, opts: &B::Opts| {
            BoundedCompiledPipeline::run(&compiled, opts, cfg)
                .map_err(|errs| MultiError::wrap(errs, name))
        };
        builder.config_validator(validator)
    }
}
