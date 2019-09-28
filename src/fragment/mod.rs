//! Fragments of configuration.
//!
//! There are several crates that provide common fragments of configuration to configure some
//! specific functionality. These fragments are described in terms of the [`Fragment`] trait.
//!
//! # How to use them
//!
//! A [`Fragment`] can be used directly, usually through [`create`] method. The trait has some
//! other methods and associated types, but these are usually used only internally.
//!
//! The other option is to use fragments through [`Pipeline`]s. A pipeline describes how the
//! fragment is extracted from the configuration, how instances of resources it describes are
//! cached and created, how to post-process them and how to install or activate them. Depending on
//! the kind of fragment, less or more tweaking may be needed or desirable.
//!
//! # How pipelines work
//!
//! A [`Pipeline`] manages certain fragment of configuration and creates [`Resource`]s out of it.
//! There are several traits in play there.
//!
//! * A pipeline is triggered whenever new configuration is loaded. It runs its configured
//!   [`Extractor`]. This extractor provides an instance of the [`Fragment`].
//! * A [`Driver`] is provided with the extracted fragment. The [`Driver`] decides if the
//!   [`Resource`] needs to be recreated or if an old instance need to be destroyed. Each fragment
//!   has its default [`Driver`], but the driver of a pipeline can be manually replaced by other
//!   one, to for example change caching strategy. Note that the [`Driver`] is allowed to partition
//!   the [`Fragment`] into smaller [`Fragment`]s and drive the rest of the pipeline multiple times
//!   on the smaller ones (eg. a `Vec<F>` would be split into multiple runs over `F`).
//! * When creating the [`Resource`], there are two stages. First, a [`Seed`] is created with the
//!   [`make_seed`] method. Then one or more [`Resource`]s are made out of the [`Seed`] with the
//!   [`make_resource`] method. This allows more flexible caching strategies and allows for example
//!   to change attached configuration without closing and opening a network socket if the port
//!   haven't changed (which would be problematic, as spirit first tries to create the new instance
//!   and only if it works gets rid of the old one ‒ but that couldn't be done if we still had the
//!   old with the same port). Some [`Fragment`]s don't need this two-stage configuration,
//!   therefore they have the [`Seed`] set to `()` and trivial [`make_seed`] method. You can use
//!   the [`simple_fragment`](../macro.simple_fragment.html) macro to generate such trait
//!   configuration.
//! * Then the resource goes through configured set of [`Transformation`]s. These may be quite
//!   arbitrary, but they usually tie the resource with some kind of functionality ‒ a network
//!   socket is provided with a function to handle each new connection, a HTTP server is provided
//!   with the service it'll serve, etc. A freshly created [`Pipeline`] has no transformations, but
//!   they can be added.
//! * Finally, the resulting product is installed using the [`Installer`]. Some fragments come with
//!   a default installer, some do not. The [`Installer`] is often set as part of a transformation.
//!   Nevertheless, an installer can always be set manually.
//! * The installer returns [`UninstallHandle`]s. These represent the lifetime of the installed
//!   resources. The pipeline stores them until the time is right to destroy the resources ‒ then
//!   it drops the handles, which results in removal of the resources.
//!
//! # Names
//!
//! Most methods around the mentioned traits take a `name: &'static str` parameter. This is used by
//! them to enrich log messages, as there might be multiple distinct parts of configuration of the
//! same type.
//!
//! The name is provided when creating the [`Pipeline`]. It needs to be a string literal, but as
//! this should correspond to specific functionality in the program, this should not be very
//! limiting.
//!
//! TODO: An example
//!
//! # How to create a fragment
//!
//! First, try to do it manually, without fragments or pipeline ‒ eg. write the code that takes the
//! configuration and creates something out of it and activates it.
//!
//! Then decide how this should be reloaded when new configuration appears, how it can be
//! reinstalled or if and how it should be cached.
//!
//! Then you can have a look at available pieces, like ready-made
//! [drivers][crate::fragment::driver] or installers. Sometimes, they come from another trait ‒ eg.
//! the [`spirit_tokio`] crate comes with an installer for futures. Usually, you need to implement
//! only the [`Fragment`] trait (either in two-stage or single-stage fashion), but sometimes you
//! might need to add some kind of [`Transformation`] to tie the part that comes from the
//! configuration with some actual code.
//!
//! You may also want to implement the [`Stackable`] and possibly [`Comparable`] traits for the
//! fragment.
//!
//! [`Fragment`]: crate::fragment::Fragment
//! [`Driver`]: crate::fragment::driver::Driver
//! [`create`]: crate::fragment::Fragment::create
//! [`Pipeline`]: crate::fragment::pipeline::Pipeline
//! [`Extractor`]: crate::fragment::Extractor
//! [`Resource`]: crate::fragment::Fragment::Resource
//! [`Seed`]: crate::fragment::Fragment::Seed
//! [`make_seed`]: crate::fragment::Fragment::make_seed
//! [`make_resource`]: crate::fragment::Fragment::make_resource
//! [`simple_fragment`]: crate::simple_fragment
//! [`Transformation`]: crate::fragment::Transformation
//! [`Installer`]: crate::fragment::Installer
//! [`UninstallHandle`]: crate::fragment::Installer::UninstallHandle
//! [`Stackable`]: crate::fragment::Stackable
//! [`Comparable`]: crate::fragment::driver::Comparable
//! [`spirit_tokio`]: https://docs.rs/spirit-tokio
use std::collections::{BTreeSet, BinaryHeap, HashSet, LinkedList};
use std::hash::{BuildHasher, Hash};

use log::trace;
use serde::de::DeserializeOwned;
use structopt::StructOpt;

use self::driver::{Driver, RefDriver, SeqDriver};
use crate::extension::Extensible;
use crate::AnyError;

pub mod driver;
pub mod pipeline;

/// An entity that is able to install a resource.
///
/// At the end of a [`Pipeline`][crate::fragment::pipeline::Pipeline] there's an installer. It
/// takes the (transformed) resource and somehow makes it active in the program.
///
/// An installer can be even a storage provided by a user where the resource is stored ‒ eg. a
/// proxy object to the resource where it can be switched.
///
/// Note that installation of the resource must not fail.
pub trait Installer<Resource, O, C> {
    /// A handle representing lifetime of the resource.
    ///
    /// Some resources or installers are for a single instance. In that case a new resource simply
    /// replaces the old one and the `UninstallHandle` serves no role and can be set to `()`.
    ///
    /// In other cases it is possible to have multiple instances of the `Resource` active at the
    /// same time (eg. futures in the tokio runtime). Then the installer returns a handle for each
    /// resource installed. The [`Pipeline`] uses the handle as a proxy to the installed resource.
    /// When the time comes for the resource to go away, the [`Pipeline`] drops the respective
    /// handle and that should take care of removing the resource.
    ///
    /// # See also
    ///
    /// The [`Stackable`] marker trait describes if it makes sense to have multiple instances of
    /// the resource, therefore if using collections of the [`Fragment`] in the configuration makes
    /// sense and is allowed.
    ///
    /// [`Pipeline`]: crate::fragment::pipeline::Pipeline
    type UninstallHandle: Send + 'static;

    /// Installs another instance of the resource.
    ///
    /// This is the main method of the trait.
    ///
    /// The installation must not fail. Depending on the resource semantics, this should either
    /// replace the previous instance or return relevant
    /// [`UninstallHandle`][Installer::UninstallHandle].
    fn install(&mut self, resource: Resource, name: &'static str) -> Self::UninstallHandle;

    /// Initialize the installer.
    ///
    /// The pipeline will run this method exactly once, upon being inserted into a
    /// [`Builder`][crate::Builder] or [`Spirit`][crate::Spirit]. This happens before any resources
    /// are installed.
    ///
    /// The installer may set up the [`Extensible`][crate::Extensible] in a suitable way.
    ///
    /// It is not mandatory to implement this method. The default installation does nothing (as
    /// many installers simply don't need any special setup).
    fn init<B: Extensible<Opts = O, Config = C, Ok = B>>(
        &mut self,
        builder: B,
        _name: &'static str,
    ) -> Result<B, AnyError>
    where
        B::Config: DeserializeOwned + Send + Sync + 'static,
        B::Opts: StructOpt + Send + Sync + 'static,
    {
        Ok(builder)
    }
}

/// A sequence installer.
///
/// This is an auxiliary installer wrapper, to install instances from a collection of fragments.
///
/// Usually, this is used behind the scenes in things like `Vec<F: Fragment>` and shouldn't have to
/// be used by user directly.
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
    fn install(&mut self, resource: Resource, name: &'static str) -> Self::UninstallHandle {
        resource
            .into_iter()
            .map(|r| self.slave.install(r, name))
            .collect()
    }
    fn init<B: Extensible<Opts = O, Config = C, Ok = B>>(
        &mut self,
        builder: B,
        name: &'static str,
    ) -> Result<B, AnyError>
    where
        B::Config: DeserializeOwned + Send + Sync + 'static,
        B::Opts: StructOpt + Send + Sync + 'static,
    {
        self.slave.init(builder, name)
    }
}

/// A trait to mark [`Fragment`]s that can form collections.
///
/// If it makes sense to use collections of the fragment (eg. `Vec<F>` or `HashSet<F>`) in the
/// configuration, in addition to implementing the [`Fragment`] trait, mark the fragment by this
/// trait. Then a default implementation of [`Fragment`] will be provided for the common
/// collections.
pub trait Stackable {}

/// A trait similar to [`Stackable`], but marking the ability to be optional.
///
/// This allows using the [`Fragment`] as `Option<F>`. This is automatically implemented for all
/// [`Stackable`] fragments.
pub trait Optional {}

impl<F: Stackable> Optional for F {}

/// A fragment of configuration.
///
/// The fragment is the part of configuration [`Pipeline`][pipeline::Pipeline]s work with. It
/// usually comes directly from the configuration, but it also may be constructed by the
/// [`Extractor`] (maybe by combining parts of the configuration, or combining data from
/// configuration and command line options).
///
/// See the details of how [pipelines work](index.html#how-pipelines-work).
///
/// Note that fragments as described by this trait create their resources in two stages ‒ first a
/// [`Seed`] is created, then turned into [`Resource`]. The [`Driver`] controls this. Note that one
/// [`Seed`] may be used to create multiple instances of the [`Resource`] (depending on the driver
/// either in parallel or sequentially, replacing the previous ones). However, if fragment doesn't
/// want to have this two-phase creation, it can set the [`Seed`] to `()`.
///
/// [`Seed`]: Fragment::Seed
/// [`Resource`]: Fragment::Resource
pub trait Fragment: Sized {
    /// The default driver to be used by the fragment.
    ///
    /// If a pipeline is created with this fragment, this is the driver that will be used by
    /// default, unless the user overrides it. There's a list of [drivers][driver] to pick from if
    /// you don't want to write your own (the most common caching and creation needs should be
    /// covered).
    type Driver: Driver<Self> + Default;

    /// The default installer to be used unless a transformation or the user doesn't provide one.
    ///
    /// This is the type the pipeline will use for installation of resources created by this
    /// fragment. Note that some fragments don't have a default installer (because they either need
    /// to be put into a user-provided storage or need to be transformed first). They can have the
    /// `Installer` set to `()` ‒ but the pipeline will not be usable unless an installer is
    /// provided by some means later on.
    type Installer: Default;

    /// The intermediate product if the fragment supports two-stage creation of
    /// [`Resource`][Fragment::Resource]s. If not, it can be set to `()`.
    type Seed;

    /// The actual product this [`Fragment`] creates.
    type Resource;

    /// Configuration if the pipeline should be run once even before the config is loaded.
    ///
    /// If this is set to true, the pipeline will be run once immediately after loading the command
    /// line options, even before the initial configuration is ready (therefore the fragment will
    /// come from within the default configuration).
    ///
    /// This does not stop the pipeline to run again once the configuration is loaded, but it may
    /// be used to provide some early intermediate setup.
    ///
    /// This is used with for example logging, as it:
    ///
    /// * Initializes very basic logging directly from the [`Installer::init`] method.
    /// * Initializes logging based on the command line only once that becomes available.
    /// * Switches to logging based on the configuration once that is available.
    ///
    /// However, most „normal“ fragments don't need to worry about this and can wait for real
    /// configuration to become available.
    const RUN_BEFORE_CONFIG: bool = false;

    /// Runs the first stage of creation.
    ///
    /// This creates the [`Seed`][Fragment::Seed]. If the two-stage creation is not needed for this
    /// fragment, this should simply return `Ok(())`.
    ///
    /// This method should be provided by an implementation, but wouldn't usually be called
    /// directly by the user. This is used either by the [`Pipeline`][pipeline::Pipeline] or
    /// internally by the higher-level [`create`][Fragment::create] method.
    fn make_seed(&self, name: &'static str) -> Result<Self::Seed, AnyError>;

    /// Runs the second stage of creation.
    ///
    /// This turns the seed into the actual resource. If the two-stage configuration is not
    /// supported by this fragment, the real work should happen in this method.
    ///
    /// This method needs to be provided by the implementation, but it wouldn't usually be called
    /// directly by the user. It is used internally by [`Pipeline`][pipeline::Pipeline] and the
    /// [`create`][Fragment::create] method.
    fn make_resource(
        &self,
        seed: &mut Self::Seed,
        name: &'static str,
    ) -> Result<Self::Resource, AnyError>;

    /// Runs both stages of creation at once.
    ///
    /// This runs both [`make_seed`][Fragment::make_seed] and
    /// [`make_resource`][Fragment::make_resource]. It should *not* be overridden by an
    /// implementation.
    ///
    /// This is meant to be used by the user if the user wishes to use the fragment directly,
    /// without the support of [`Pipeline`][pipeline::Pipeline].
    fn create(&self, name: &'static str) -> Result<Self::Resource, AnyError> {
        trace!("End to end creation of {}", name);
        let mut seed = self.make_seed(name)?;
        self.make_resource(&mut seed, name)
    }

    /// An initialization routine.
    ///
    /// This will be called once by a [`Pipeline`][pipeline::Pipeline] before the first use of the
    /// fragment. This allows the fragment to configure the [`Builder`][crate::Builder] or
    /// [`Spirit`][crate::Spirit] the pipeline will be used in.
    ///
    /// The implementation may leave it at the default (empty) implementation in case no special
    /// setup is needed.
    fn init<B: Extensible<Ok = B>>(builder: B, _: &'static str) -> Result<B, AnyError>
    where
        B::Config: DeserializeOwned + Send + Sync + 'static,
        B::Opts: StructOpt + Send + Sync + 'static,
    {
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
    fn make_seed(&self, name: &'static str) -> Result<Self::Seed, AnyError> {
        F::make_seed(*self, name)
    }
    fn make_resource(
        &self,
        seed: &mut Self::Seed,
        name: &'static str,
    ) -> Result<Self::Resource, AnyError> {
        F::make_resource(*self, seed, name)
    }
    fn init<B: Extensible<Ok = B>>(builder: B, name: &'static str) -> Result<B, AnyError>
    where
        B::Config: DeserializeOwned + Send + Sync + 'static,
        B::Opts: StructOpt + Send + Sync + 'static,
    {
        F::init(builder, name)
    }
}

// TODO: Export the macro for other containers?
// TODO: The where-* should be where-?
macro_rules! fragment_for_seq {
    ($cond: ident => $container: ident<$base: ident $(, $extra: ident)*> $(where $($bounds: tt)+)*) => {
        impl<$base: Fragment + $cond + 'static $(, $extra)*> Fragment
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
            fn make_seed(&self, name: &'static str) -> Result<Self::Seed, AnyError> {
                self.iter().map(|i| i.make_seed(name)).collect()
            }
            fn make_resource(&self, seed: &mut Self::Seed, name: &'static str)
                -> Result<Self::Resource, AnyError>
            {
                self.iter()
                    .zip(seed)
                    .map(|(i, s)| i.make_resource(s, name))
                    .collect()
            }
            fn init<B: Extensible<Ok = B>>(builder: B, name: &'static str) -> Result<B, AnyError>
            where
                B::Config: DeserializeOwned + Send + Sync + 'static,
                B::Opts: StructOpt + Send + Sync + 'static,
            {
                $base::init(builder, name)
            }
        }
    }
}

fragment_for_seq!(Stackable => Vec<T>);
fragment_for_seq!(Stackable => BTreeSet<T>);
fragment_for_seq!(Stackable => LinkedList<T>);
fragment_for_seq!(Stackable => BinaryHeap<T> where T: Ord);
fragment_for_seq!(Stackable => HashSet<T, S> where T: Eq + Hash, S: BuildHasher);
fragment_for_seq!(Optional => Option<T>);

/// A helper macro to implement a simple [`Fragment`].
///
/// The full implementation of a [`Fragment`] requires a lot of work that is not usually needed.
///
/// In case the [`Fragment`] should not implement the two-stage creation, this can be used to cut
/// down on the boilerplate a bit.
///
/// # Examples
///
/// ```rust
/// use spirit::AnyError;
/// use spirit::simple_fragment;
/// use spirit::fragment::Installer;
///
/// // The message as a resource
/// struct Message(String);
///
/// // An installer of a resource
/// #[derive(Default)]
/// struct MessageInstaller;
///
/// impl<O, C> Installer<Message, O, C> for MessageInstaller {
///     type UninstallHandle = ();
///     fn install(&mut self, message: Message, _name: &str) {
///         println!("{}", message.0);
///     }
/// }
///
/// // Configuration of a message from the config file
/// # #[allow(dead_code)] // Allow not using this structure.
/// struct MessageCfg {
///     msg: String,
/// }
///
/// simple_fragment! {
///     impl Fragment for MessageCfg {
///         type Resource = Message;
///         type Installer = MessageInstaller;
///         fn create(&self, _name: &'static str) -> Result<Message, AnyError> {
///             Ok(Message(self.msg.clone()))
///         }
///     }
/// }
/// # fn main() {}
/// ```
///
/// If the [`Driver`] is not provided (as in the above example), the [trivial
/// driver][crate::fragment::driver::Trivial] is going to be used.
///
/// [`Fragment`]: crate::fragment::Fragment
/// [`Driver`]: crate::fragment::driver::Driver
#[macro_export]
macro_rules! simple_fragment {
    (impl Fragment for $ty: ty {
        type Resource = $resource: ty;
        type Installer = $installer: ty;
        fn create(&$self: tt, $name: tt: &'static str) -> $result: ty $block: block
    }) => {
        $crate::simple_fragment! {
            impl Fragment for $ty {
                type Driver = $crate::fragment::driver::Trivial;
                type Resource = $resource;
                type Installer = $installer;
                fn create(&$self, $name: &'static str) -> $result $block
            }
        }
    };
    (impl Fragment for $ty: ty {
        type Driver = $driver: ty;
        type Resource = $resource: ty;
        type Installer = $installer: ty;
        fn create(&$self: tt, $name: tt: &'static str) -> $result: ty $block: block
    }) => {
        impl $crate::fragment::Fragment for $ty {
            type Driver = $driver;
            type Resource = $resource;
            type Installer = $installer;
            type Seed = ();
            fn make_seed(&self, _: &'static str) -> Result<(), AnyError> {
                Ok(())
            }
            fn make_resource(&$self, _: &mut (), $name: &'static str) -> $result $block
        }
    }
}

// TODO: How do we stack maps, etc?
// TODO: Arcs, Rcs, Mutexes, refs, ...

/// A trait describing something that extracts a fragment from configuration and command line
/// options.
///
/// The extractor is used every time a [`Pipeline`] is triggered, to get the
/// fragment out.
///
/// Usually, an extractor is a closure, but something else can implement the trait too.
///
/// Users usually don't need to interact with this trait directly.
///
/// Note that the extractor is lifetime-parametric. Usually the real extractor implements the trait
/// for all lifetimes (and it is not useful otherwise). This allows returning references into the
/// configuration, the [`Pipeline`] is able to work with that (given new enough `rustc` ‒ there
/// were some bugs preventing it from working; if that's the case, you can clone and return owned
/// values).
///
/// [`Pipeline`]: pipeline::Pipeline
pub trait Extractor<'a, O, C> {
    /// The fragment being extracted.
    type Fragment: Fragment + 'a;

    /// The actual call of the extractor.
    ///
    /// The extractor is allowed to either reference into the configuration (or command line
    /// options) or create something new out of it (including structures containing references
    /// there).
    ///
    /// It is not uncommon to combine information from both to form a fragment.
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

/// A transformation of resources.
///
/// A [`Pipeline`] can contain a transformation of the [`Resource`] produced by the [`Fragment`].
/// This trait describes a transformation.
///
/// Note that transformations in a pipeline are usually composed together.
///
/// The transformation is also allowed (and sometimes required) to either transform or replace the
/// [`Installer`] of the [`Pipeline`]. The old [`Installer`] might not be able to install the new
/// [`Resource`] (since the type can change during the transformation) or might not even exist.
///
/// A transformation can be quite arbitrary thing that simply takes the old resource and produces
/// something new. But more often that not, it is used to somehow make a „dead“ resource (eg. a
/// network socket) „alive“ (eg. wrap it into a future that is then installed) ‒ or, in other
/// words, to tie the resource to some functionality.
///
/// It is also possible to use a transformation to post-process and tweak the resource a bit (add
/// more loggers, change a bit of configuration of the resource, ...).
///
/// # Type parameters
///
/// * `InputResource` is the resource on the transformation input; this one will be changed every
///   time the transformation is called.
/// * `InputInstaller` is the original installer that was present in the pipeline before the
///   transformation got added. Note that not all installers at that point are able to install the
///   `InputResource`. But if it is able, it can be used to delegate from the
///   [`OutputInstaller`][Transformation::OutputInstaller] (or just use it vanilla).
/// * `SubFragment` is the fragments the [`Transformation`] will be used on. Each
///   [`Resource`][Fragment::Resource] is always accompanied by the [`Fragment`] it came from. Note
///   that this might be some sub-part of the original [`Fragment`], as the [`Driver`] is allowed
///   to cut it into smaller pieces.
///
/// [`Resource`]: Fragment::Resource
/// [`Pipeline`]: pipeline::Pipeline
pub trait Transformation<InputResource, InputInstaller, SubFragment> {
    /// The type of resource after the transformation.
    type OutputResource: 'static;

    /// The type of installer after the transformation.
    ///
    /// This can be something completely new, or something that somehow delegates or uses the
    /// `InputResource`.
    type OutputInstaller;

    /// Creates the installer.
    ///
    /// This is called by the pipeline exactly once, with the original installer, to produce the
    /// transformed installer.
    ///
    /// Note that some transformations are composed from multiple transformations and they are
    /// *not* mandated to call this method. Nevertheless, it is violation of contract to call this
    /// more than once.
    fn installer(&mut self, installer: InputInstaller, name: &'static str)
        -> Self::OutputInstaller;

    /// Transforms one instance of the resource.
    ///
    /// The `fragment` parameter was the fragment that was used to create the resource. Note that
    /// there might have been some other transformations between the creation and *this*
    /// transformation and therefore the resource produced by the `fragment` might be something
    /// else than what is being put into the transformation.
    ///
    /// Nevertheless, the transformation is allowed to look into the fragment ‒ for example to
    /// examine additional configuration not used to directly create the resource, but maybe
    /// configure the „alive“ part that this transformation adds.
    fn transform(
        &mut self,
        resource: InputResource,
        fragment: &SubFragment,
        name: &'static str,
    ) -> Result<Self::OutputResource, AnyError>;
}
