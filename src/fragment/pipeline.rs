use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

use failure::Error;
use parking_lot::Mutex;

use crate::extension::{Extensible, Extension};
use crate::validation::{Result as ValidationResult, Results as ValidationResults};
use super::{Extractor, Fragment, Installer, Transformation};
use super::driver::{CacheId, CacheInstruction, Driver};

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

#[derive(Clone, Copy, Debug, Default)]
pub struct NopTransformation;

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
