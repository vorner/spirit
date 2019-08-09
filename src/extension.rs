//! Interfaces for extending the [`Spirit`] and [`Builder`] with callbacks.
//!
//! Both the [`Spirit`] and [`Builder`] types allow registering callbacks. The [`Extensible`] trait
//! describes the interface for registering them.
//!
//! Furthermore, [`Extensible`] is also implemented for `Result`s containing them. This allows
//! chaining the building without manually handling the errors. The error then can be handled by
//! [`SpiritBuilder::run`] and logged, without making a distinction if it comes from the setup or
//! application run.
//!
//! Some libraries might want to provide bits of functionality that need to register several
//! different callbacks at once to work properly. This is described by the [`Extension`] trait.
//!
//! In addition, the module contains few useful extensions that may be plugged in.
//!
//! [`Spirit`]: crate::Spirit
//! [`Builder`]: crate::Builder
//! [`Extension`]: crate::extension::Extension

use std::any::Any;
use std::fmt::Display;
use std::sync::Arc;

use failure::Error;
use log::warn;

use crate::bodies::InnerBody;
use crate::spirit::Spirit;
use crate::validation::Action;

/// An internal trait to make working uniformly with the [`Builder`] and `Result<Builder, Error>`
/// possible.
///
/// You should not need to interact with the trait directly.
///
/// The idea is that both the [`Builder`] and the `Result<Builder, Error>` can be turned into
/// `Result<Builder, Error>`.
///
/// [`Builder`]: crate::Builder
pub trait IntoResult<T>: Sized {
    /// Turns self into the result.
    fn into_result(self) -> Result<T, Error>;
}

impl<T> IntoResult<T> for Result<T, Error> {
    fn into_result(self) -> Result<T, Error> {
        self
    }
}

impl<T> IntoResult<T> for T {
    fn into_result(self) -> Result<T, Error> {
        Ok(self)
    }
}

/// An interface allowing to extend something with callbacks.
///
/// This describes the interface to registering various callbacks. This unifies the interaction
/// with [`Builder`] and [`Spirit`], so there's little difference between registering a callback
/// when setting things up and when everything is already running (but see notes at various
/// methods, sometimes there are subtle differences ‒ specifically, it is not possible to do some
/// things when the application already started).
///
/// In addition, this is also implemented on `Result<Extensible, Error>`. This allows the caller to
/// postpone all error handling for later or even leave it up to the
/// [`Builder::run`][crate::SpiritBuilder::run`] to handle them.
///
/// # Deadlocks
///
/// In general, it is not possible to register callbacks from within callbacks. Internally, the
/// structures holding them are protected by a mutex which needs to be locked both when
/// manipulating and running them. Such attempt to register a callback from within a callback will
/// lead to a deadlock (this restriction might be lifted in the future).
///
/// # Examples
///
/// ```rust
/// use spirit::prelude::*;
///
/// // This creates a Builder
/// Spirit::<Empty, Empty>::new()
///     // This returns Result<Builder, Error>. But we don't handle the error here, it propagates
///     // further in the call chain.
///     .run_before(|_spirit| Ok(()))
///     // This run_before is on the Result<Builder, Error>. If the first one returned an error,
///     // nothing would happen and the error would thread on. So, this works like implicit .and_then,
///     // but without the inconvenience.
///     //
///     // (This also returns Result<Builder, Error>, not Result<Result<Builder, Error>, Error>).
///     .run_before(|_spirit| Ok(()))
///     // This .run can handle both the errors from above and from inside, logging them and
///     // terminating if they happen.
///     .run(|spirit| {
///         // And this callback is registered onto the Spirit. Here, the run_before is started
///         // right away.
///         //
///         // Here it is more convenient to just propagate the error, because the .run will handle
///         // it.
///         spirit.run_before(|_spirit| Ok(()))?;
///
///         Ok(())
///     });
/// ```
///
/// [`Builder`]: crate::Builder
/// [`Spirit`]: crate::Spirit
pub trait Extensible: Sized {
    /// The command line options structure tied to this instance.
    ///
    /// A lot of the callbacks take command line structure as specified by the caller. This makes
    /// the type available for users of the trait.
    type Opts;

    /// The configuration structure.
    ///
    /// Similar to [`Opts`][Extensible::Opts], this makes the type used to load configuration
    /// available to the users of the trait.
    type Config;

    /// The Ok variant used when returning a result.
    ///
    /// Part of the trick to treat both `Extensible` and `Result<Extensible, Error>` in an uniform
    /// way. This specifies what the OK variant of a result is ‒ it is either the Ok variant of
    /// `Self` if we are `Result`, or `Self` if we are the `Extensible` proper.
    type Ok;

    /// Has the application already started?
    ///
    /// This makes it possible to distinguish between the [`Builder`][crate::Builder] and
    /// [`Spirit`][crate::Spirit] in generic extension code.
    ///
    /// In general, this should not be needed as most of the method act in a sane way relevant to
    /// the given type, but some special handling may be still needed.
    const STARTED: bool;

    /// A callback that is run after the building started and the command line is parsed, but even
    /// before the first configuration is loaded. The configuration provided is either the one
    /// provided to the builder, or a default one when called on the builder, but current
    /// configuration when run on [`Spirit`].
    ///
    /// This is run right away if called on [`Spirit`], unless it is already terminated. In such
    /// case, the callback is dropped.
    ///
    /// [`Spirit`]: crate::Spirit
    fn before_config<F>(self, cback: F) -> Result<Self::Ok, Error>
    where
        F: FnOnce(&Self::Config, &Self::Opts) -> Result<(), Error> + Send + 'static;

    /// Adds another config validator to the chain.
    ///
    /// The validators are there to check and possibly refuse a newly loaded configuration.
    ///
    /// The callback is passed three parameters:
    ///
    /// * The old configuration.
    /// * The new configuration.
    /// * The command line options.
    ///
    /// The new configuration is handled in this order:
    ///
    /// * First, [config mutators][Extensible::config_mutator] are called (in order of their
    ///   registration).
    /// * Then all the config validators are called, in order of their registration. If any of them
    ///   fails, the configuration is refused and failure actions returned by the successful
    ///   validators are run.
    /// * If successful, the success actions returned by validators are run.
    /// * New configuration is stored.
    /// * Config hooks are called, in order of registration.
    ///
    /// # The actions
    ///
    /// Sometimes, the only way to validate a piece of config is to try it out ‒ like when you want
    /// to open a listening socket, you don't know if the port is free. But you can't activate and
    /// apply just yet, because something further down the configuration might still fail.
    ///
    /// So, you open the socket (or create an error result) and store it into the success action to
    /// apply it later on. If something fails, the action is dropped and the socket closed.
    ///
    /// The failure action lets you roll back (if it isn't done by simply dropping the thing).
    ///
    /// If the validation and application steps can be separated (you can check if something is OK
    /// by just „looking“ at it ‒ like with a regular expression and using it can't fail), you
    /// don't have to use them, just use verification and [`on_config`](#method.on_config)
    /// separately.
    ///
    /// If called on the already started [`Spirit`][crate::Spirit], it is run right away. If it is
    /// called on terminated one, it is dropped.
    ///
    /// # Examples
    ///
    /// TODO
    fn config_validator<F>(self, f: F) -> Result<Self::Ok, Error>
    where
        F: FnMut(&Arc<Self::Config>, &Arc<Self::Config>, &Self::Opts) -> Result<Action, Error>
            + Send
            + 'static;

    /// Adds a callback able to mutate the configuration while being loaded.
    ///
    /// Config mutators are run (in order of their registration) before [config
    /// validators](#method.config_validator) and allow the callback to modify the configuration.
    /// Note that by the time it is called it is not yet determined if this configuration is
    /// valid. A config mutator should not use the configuration in any way, only tweak it (and
    /// possibly warn about having done so), but the tweaked configuration should be used by
    /// something else down the line.
    ///
    /// If run on an already started [`Spirit`][crate::Spirit], this only registers the callback
    /// but doesn't call it. If run on terminated one, it is dropped.
    fn config_mutator<F>(self, f: F) -> Self
    where
        F: FnMut(&mut Self::Config) + Send + 'static;

    /// Adds a callback for notification about new configurations.
    ///
    /// The callback is called once a new configuration is loaded and successfully validated, so it
    /// can be directly used.
    ///
    /// It is run right away on a started [`Spirit`][crate::Spirit], but it is dropped if it was
    /// already terminated.
    fn on_config<F>(self, hook: F) -> Self
    where
        F: FnMut(&Self::Opts, &Arc<Self::Config>) + Send + 'static;

    /// Adds a callback for reacting to a signal.
    ///
    /// The [`Spirit`][crate::Spirit] reacts to some signals itself, in its own service
    /// thread. However, it is also possible to hook into any signals directly (well, any except
    /// the ones that are [off limits][signal_hook::FORBIDDEN]).
    ///
    /// These are not run inside the real signal handler, but are delayed and run in the service
    /// thread. Therefore, restrictions about async-signal-safety don't apply to the hook.
    ///
    /// It is dropped if called on already terminated spirit.
    ///
    /// # Panics
    ///
    /// This may panic in case the application runs without the background signal thread. See
    /// [`SpiritBuilder::build`][crate::SpiritBuilder::build].
    ///
    /// TODO: Threads, deadlocks
    fn on_signal<F>(self, signal: libc::c_int, hook: F) -> Result<Self::Ok, Error>
    where
        F: FnMut() + Send + 'static;

    /// Adds a callback executed once the [`Spirit`] decides to terminate.
    ///
    /// This is called either when someone calls [`terminate`](struct.Spirit.html#method.terminate)
    /// or when a termination signal is received.
    ///
    /// Note that there are ways the application may terminate without calling these hooks ‒ for
    /// example terminating the main thread, or aborting.
    ///
    /// If called on already started and *terminated* [`Spirit`], it is run right away.
    ///
    /// [`Spirit`]: crate::Spirit.
    fn on_terminate<F>(self, hook: F) -> Self
    where
        F: FnOnce() + Send + 'static;

    /// Add a closure run before the main body.
    ///
    /// The [`run`][crate::SpiritBuilder::run] will first execute all closures submitted through
    /// this method before running the real body. They are run in the order of registration.
    ///
    /// The purpose of this is mostly integration with extensions ‒ they often need some last minute
    /// preparation.
    ///
    /// In case of using only [`build`][crate::SpiritBuilder::build], the bodies are composed into
    /// one object and returned as part of the [`App`][crate::app::App].
    ///
    /// If called on an already started [`Spirit`][crate::Spirit], it is run immediately and any
    /// error returned from it is propagated.
    ///
    /// If submitted to the [`Builder`][crate::Builder], the first body to fail terminates the
    /// processing and further bodies (including the main one) are skipped.
    ///
    /// # Examples
    ///
    /// ```
    /// # use spirit::prelude::*;
    /// Spirit::<Empty, Empty>::new()
    ///     .run_before(|_spirit| {
    ///         println!("Run first");
    ///         Ok(())
    ///     }).run(|_spirit| {
    ///         println!("Run second");
    ///         Ok(())
    ///     });
    /// ```
    fn run_before<B>(self, body: B) -> Result<Self::Ok, Error>
    where
        B: FnOnce(&Arc<Spirit<Self::Opts, Self::Config>>) -> Result<(), Error> + Send + 'static;

    /// Wrap the body run by the [`run`][crate::SpiritBuilder::run] into this closure.
    ///
    /// The inner body is passed as an object with a [`run`][crate::bodies::InnerBody]
    /// method, not as a closure, due to a limitation around boxed `FnOnce`.
    ///
    /// It is expected the wrapper executes the inner body as part of itself and propagates any
    /// returned error.
    ///
    /// In case of multiple wrappers, the ones submitted later on are placed inside the sooner
    /// ones ‒ the first one is the outermost.
    ///
    /// In case of using only [`build`][crate::SpiritBuilder::build], all the wrappers composed
    /// together are returned as part of the result.
    ///
    /// # Panics
    ///
    /// If called on the already started [`crate::Spirit`], this panics. It is part of the
    /// interface to make it possible to write extensions that register a body wrapper as part of
    /// an singleton, but assume the singleton would be plugged in by the time it is already
    /// started.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use spirit::prelude::*;
    /// Spirit::<Empty, Empty>::new()
    ///     .run_around(|_spirit, inner| {
    ///         println!("Run first");
    ///         inner.run()?;
    ///         println!("Run third");
    ///         Ok(())
    ///     }).run(|_spirit| {
    ///         println!("Run second");
    ///         Ok(())
    ///     });
    /// ```
    fn run_around<W>(self, wrapper: W) -> Result<Self::Ok, Error>
    where
        W: FnOnce(&Arc<Spirit<Self::Opts, Self::Config>>, InnerBody) -> Result<(), Error>
            + Send
            + 'static;

    /// Apply an [`Extension`].
    ///
    /// An extension is allowed to register arbitrary amount of callbacks.
    fn with<E>(self, ext: E) -> Result<Self::Ok, Error>
    where
        E: Extension<Self::Ok>;

    /// Check if this is the first call with the given type.
    ///
    /// Some helpers share common part. This common part makes sense to register just once, so this
    /// can be used to check that. The first call with given type returns `true`, any future ones
    /// with the same type return `false`.
    ///
    /// The method has no direct effect on the future spirit constructed from the builder and
    /// works only as a note for future helpers that want to manipulate the builder.
    ///
    /// A higher-level interface is the [`with_singleton`](#method.with_singleton) method.
    ///
    /// Singletons registered into a [`Builder`][crate::Builder] are then inherited into the
    /// [`Spirit`][crate::Spirit], therefore they can be registered once during the whole lifetime.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use spirit::prelude::*;
    ///
    /// let mut builder = Spirit::<Empty, Empty>::new();
    ///
    /// struct X;
    /// struct Y;
    ///
    /// assert!(builder.singleton::<X>());
    /// assert!(!builder.singleton::<X>());
    /// assert!(builder.singleton::<Y>());
    /// ```
    fn singleton<T: 'static>(&mut self) -> bool;

    /// Applies the first [`Extension`] of the same type.
    ///
    /// This applies the passed extension, but only if an extension with the type same hasn't yet
    /// been applied (or the [`singleton`](#method.singleton) called manually).
    ///
    /// Note that different instances of the same type of an extension can act differently, but are
    /// still considered the same type. This means the first instance wins. This is considered a
    /// feature ‒ many other extensions need some environment to run in (like `tokio` runtime). The
    /// extensions try to apply a default configuration, but the user can apply a specific
    /// configuration first.
    fn with_singleton<T>(self, singleton: T) -> Result<Self::Ok, Error>
    where
        T: Extension<Self::Ok> + 'static;

    /// Keeps a guard object until destruction.
    ///
    /// Sometimes, some things (like, a logger that needs flushing, metrics collector handle) need
    /// a guard that is destroyed at the end of application lifetime.  However, the [`run`] body
    /// may terminate sooner than the application, therefore destroying the guards too soon.
    ///
    /// By passing the ownership to [`Spirit`], the guard is destroyed together with the [`Spirit`]
    /// itself.
    ///
    /// Note that you may need to [wait for the background thread] or [autojoin
    /// it][Extensible::autojoin_bg_thread].
    ///
    /// The guards can be only added (there's no way to remove or overwrite them later on).
    ///
    /// [`run`]: crate::SpiritBuilder::run
    /// [`Spirit`]: crate::Spirit
    /// [wait for the background thread]: crate::Spirit::join_bg_thread
    fn keep_guard<G: Any + Send>(self, guard: G) -> Self;

    /// Specifies that the background thread should be joined automatically, as part of the [`run`]
    /// method.
    ///
    /// [`run`]: crate::SpiritBuilder::run
    fn autojoin_bg_thread(self) -> Self;
}

impl<C> Extensible for Result<C, Error>
where
    C: Extensible<Ok = C>,
{
    type Opts = C::Opts;
    type Config = C::Config;
    type Ok = C;
    const STARTED: bool = C::STARTED;

    fn before_config<F>(self, cback: F) -> Result<<Self as Extensible>::Ok, Error>
    where
        F: FnOnce(&Self::Config, &Self::Opts) -> Result<(), Error> + Send + 'static,
    {
        self.and_then(|c| c.before_config(cback))
    }

    fn config_mutator<F>(self, f: F) -> Self
    where
        F: FnMut(&mut Self::Config) + Send + 'static,
    {
        self.map(|c| c.config_mutator(f))
    }

    fn config_validator<F>(self, f: F) -> Result<<Self as Extensible>::Ok, Error>
    where
        F: FnMut(&Arc<Self::Config>, &Arc<Self::Config>, &Self::Opts) -> Result<Action, Error>
            + Send
            + 'static,
    {
        self.and_then(|c| c.config_validator(f))
    }

    fn on_config<F>(self, hook: F) -> Self
    where
        F: FnMut(&Self::Opts, &Arc<Self::Config>) + Send + 'static,
    {
        self.map(|c| c.on_config(hook))
    }

    fn on_signal<F>(self, signal: libc::c_int, hook: F) -> Result<<Self as Extensible>::Ok, Error>
    where
        F: FnMut() + Send + 'static,
    {
        self.and_then(|c| c.on_signal(signal, hook))
    }

    fn on_terminate<F>(self, hook: F) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        self.map(|c| c.on_terminate(hook))
    }

    fn run_before<B>(self, body: B) -> Result<<Self as Extensible>::Ok, Error>
    where
        B: FnOnce(&Arc<Spirit<Self::Opts, Self::Config>>) -> Result<(), Error> + Send + 'static,
    {
        self.and_then(|c| c.run_before(body))
    }

    fn run_around<W>(self, wrapper: W) -> Result<<Self as Extensible>::Ok, Error>
    where
        W: FnOnce(&Arc<Spirit<Self::Opts, Self::Config>>, InnerBody) -> Result<(), Error>
            + Send
            + 'static,
    {
        self.and_then(|c| c.run_around(wrapper))
    }

    fn with<E>(self, ext: E) -> Result<<Self as Extensible>::Ok, Error>
    where
        E: Extension<<Self as Extensible>::Ok>,
    {
        self.and_then(|c| c.with(ext))
    }

    fn singleton<T: 'static>(&mut self) -> bool {
        // If we are errored out, this doesn't really matter, but usually false means less work to
        // do.
        self.as_mut()
            .map(Extensible::singleton::<T>)
            .unwrap_or_default()
    }

    fn with_singleton<T>(self, singleton: T) -> Result<<Self as Extensible>::Ok, Error>
    where
        T: Extension<<Self as Extensible>::Ok> + 'static,
    {
        self.and_then(|c| c.with_singleton(singleton))
    }

    fn keep_guard<G: Any + Send>(self, guard: G) -> Self {
        self.map(|s| s.keep_guard(guard))
    }

    fn autojoin_bg_thread(self) -> Self {
        self.map(Extensible::autojoin_bg_thread)
    }
}

/// The basic extension trait.
///
/// It allows being plugged into a [`Builder`][crate::Builder] or [`Spirit`][crate::Spirit] and
/// modifying it in an arbitrary way.
///
/// It is more common to apply the helper by the
/// [`with`][Extensible::with] or [`with_singleton`][Extensible::with_singleton] method than
/// directly.
///
/// There's an implementation of `Extension` for `FnOnce(Extensible) -> Result<Extensible, Error>`,
/// so extensions can be either custom types or just closures (which are often more convenient than
/// defining an empty type and the implementation).
///
/// It is better to define the extension in a generic way (eg. accepting any type that is
/// [`Extensible`]) instead of eg. a [`Builder`][crate::Builder] directly. Note that sometimes it
/// is needed to restrict the acceptable [`Extensible`]s to the base ones, with `Ok = E`, like in
/// the example below. They'll still be possible to apply to `Result`s.
///
/// # Examples
///
/// ```rust
/// use failure::Error;
/// use spirit::extension::Extension;
/// use spirit::prelude::*;
///
/// struct CfgPrint;
///
/// impl<E: Extensible<Ok = E>> Extension<E> for CfgPrint {
///     fn apply(self, ext: E) -> Result<E, Error> {
///         Ok(ext.on_config(|_opts, _config| println!("Config changed")))
///     }
/// }
///
/// Spirit::<Empty, Empty>::new()
///     .with(CfgPrint)
///     .run(|_spirit| {
///         println!("Running...");
///         Ok(())
///     })
/// ```
///
/// ```rust
/// use spirit::prelude::*;
///
/// fn cfg_print<E: Extensible<Ok = E>>(ext: E) -> E {
///     ext.on_config(|_opts, _config| println!("Config changed"))
/// }
///
/// Spirit::<Empty, Empty>::new()
///     .with(cfg_print)
///     .run(|_spirit| {
///         println!("Running...");
///         Ok(())
///     })
/// ```
pub trait Extension<B> {
    /// Perform the transformation on the given extensible.
    ///
    /// And yes, it is possible to do multiple primitive transformations inside one extension (this
    /// is what makes extensions useful for 3rd party crates, they can integrate with just one call
    /// of [`with`][Extensible::with]).
    fn apply(self, builder: B) -> Result<B, Error>;
}

impl<B, F, R> Extension<B> for F
where
    F: FnOnce(B) -> R,
    R: IntoResult<B>,
{
    fn apply(self, builder: B) -> Result<B, Error> {
        self(builder).into_result()
    }
}

/// An extension for one-time initial configuration.
///
/// Sometimes, some configuration values can't be reasonably updated at runtime (libraries don't
/// support reconfiguration, there's no time to implement that, ...). This callback tries to
/// improve the situation around these configurations.
///
/// The `extractor` extracts a fragment of configuration every time a configuration is loaded. The
/// first time this happens, `init` is called with this extracted configuration. Upon any future
/// configuration reloads, a warning is issued (with the given `name`) if the configuration
/// contains a different value than the one it was originally initialized (and is silent if it is
/// the same).
///
/// # See also
///
/// * [`immutable_cfg`]
///
/// # Examples
///
/// ```
/// use serde::Deserialize;
/// use spirit::prelude::*;
/// use spirit::extension;
///
/// #[derive(Clone, Debug, Default, Deserialize)]
/// struct Cfg {
///     #[serde(default)]
///     msg: String,
/// }
///
/// impl Cfg {
///     fn msg(&self) -> &String {
///         &self.msg
///     }
/// }
///
/// fn print_msg(msg: &String) {
///     println!("{}", msg);
/// }
///
/// fn main() {
///     Spirit::<Empty, Cfg>::new()
///         // The first version of `msg` is printed at the initial configuration load. If however
///         // the configuration changes into some other message, a warning is printed (because
///         // there's no way to modify the already printed message
///         .with(extension::immutable_cfg_init(Cfg::msg, print_msg, "message"))
///         .run(|_| Ok(()));
/// }
/// ```
pub fn immutable_cfg_init<Ext, R, E, F, N>(extractor: E, init: F, name: N) -> impl Extension<Ext>
where
    Ext: Extensible,
    E: for<'a> Fn(&'a Ext::Config) -> &R + Send + 'static,
    F: FnOnce(&R) + Send + 'static,
    R: Clone + PartialEq + Send + 'static,
    N: Display + Send + 'static,
{
    let mut first = None;
    let mut init = Some(init);
    let on_cfg = move |_o: &Ext::Opts, c: &Arc<Ext::Config>| {
        let extracted = extractor(&c);
        if first.is_none() {
            first = Some(extracted.clone());
            (init.take().expect("Init called multiple times"))(extracted);
        } else if first.as_ref() != Some(extracted) {
            warn!("Configuration {} can't be changed at runtime", name);
        }
    };
    |ext: Ext| ext.on_config(on_cfg)
}

/// An extension to warn about changes to configuration that can't be updated at runtime.
///
/// This is similar to [`immutable_cfg_init`] except that there's no callback called at the first
/// load.
///
/// # Examples
///
/// ```
/// use serde::Deserialize;
/// use spirit::prelude::*;
/// use spirit::extension;
///
/// #[derive(Clone, Debug, Default, Deserialize)]
/// struct Cfg {
///     #[serde(default)]
///     msg: String,
/// }
///
/// impl Cfg {
///     fn msg(&self) -> &String {
///         &self.msg
///     }
/// }
///
/// fn main() {
///     Spirit::<Empty, Cfg>::new()
///         // This prints a warning if the message is ever changed during runtime ‒ we can't take
///         // it back and change it after it got printed in the body.
///         .with(extension::immutable_cfg(Cfg::msg, "message"))
///         .run(|spirit| {
///             println!("{}", spirit.config().msg);
///             Ok(())
///         });
/// }
/// ```
pub fn immutable_cfg<Ext, R, E, N>(extractor: E, name: N) -> impl Extension<Ext>
where
    Ext: Extensible,
    E: for<'a> Fn(&'a Ext::Config) -> &R + Send + 'static,
    R: Clone + PartialEq + Send + 'static,
    N: Display + Send + 'static,
{
    immutable_cfg_init(extractor, |_| (), name)
}
