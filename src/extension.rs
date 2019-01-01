//! Helpers for integrating common configuration patterns.
//!
//! There are some common patterns of integrating pieces of configuration into an application and
//! make them active. Many of these patterns require registering in multiple callbacks at once to
//! work correctly. Doing it manually is tedious and error prone.
//!
//! The traits in this module allow registering all the callbacks in one go, making it easier for
//! other crates to integrate such patterns.

use std::borrow::Borrow;
use std::fmt::{Debug, Display};
use std::sync::Arc;

use arc_swap::ArcSwap;
use failure::Error;
use log::{trace, warn};
use serde::de::DeserializeOwned;
use structopt::StructOpt;

use crate::bodies::InnerBody;
use crate::spirit::{Builder, Spirit};
use crate::validation::Results as ValidationResults;

pub trait IntoResult<T>: Sized {
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

pub trait Extensible: Sized {
    type Opts;
    type Config;
    type Ok;
    const STARTED: bool;

    /// Adds another config validator to the chain.
    ///
    /// The validators are there to check, possibly modify and possibly refuse a newly loaded
    /// configuration.
    ///
    /// The callback is passed three parameters:
    ///
    /// * The old configuration.
    /// * The new configuration (possible to modify).
    /// * The command line options.
    ///
    /// They are run in order of being set. Each one can pass arbitrary number of results, where a
    /// result can carry a message (of different severities) that ends up in logs and actions to be
    /// taken if the validation succeeds or fails.
    ///
    /// If none of the validator returns an error-level message, the validation passes. After it is
    /// determined if the configuration passed, either all the success or failure actions are run.
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
    /// TODO: Threads, deadlocks
    ///
    /// # Examples
    ///
    /// TODO
    fn config_validator<R, F>(self, f: F) -> Result<Self::Ok, Error>
    where
        F: FnMut(&Arc<Self::Config>, &mut Self::Config, &Self::Opts) -> R + Send + 'static,
        R: Into<ValidationResults>;

    /// Adds a callback for notification about new configurations.
    ///
    /// The callback is called once a new configuration is loaded and successfully validated.
    ///
    /// TODO: Threads, deadlocks
    fn on_config<F>(self, hook: F) -> Self
    where
        F: FnMut(&Self::Opts, &Arc<Self::Config>) + Send + 'static;

    /// Adds a callback for reacting to a signal.
    ///
    /// The [`Spirit`](struct.Spirit.html) reacts to some signals itself, in its own service
    /// thread. However, it is also possible to hook into any signals directly (well, any except
    /// the ones that are [off limits](https://docs.rs/signal-hook/*/signal_hook/constant.FORBIDDEN.html)).
    ///
    /// These are not run inside the real signal handler, but are delayed and run in the service
    /// thread. Therefore, restrictions about async-signal-safety don't apply to the hook.
    ///
    /// TODO: Threads, deadlocks
    fn on_signal<F>(self, signal: libc::c_int, hook: F) -> Result<Self::Ok, Error>
    where
        F: FnMut() + Send + 'static;

    /// Adds a callback executed once the [`Spirit`](struct.Spirit.html) decides to terminate.
    ///
    /// This is called either when someone calls [`terminate`](struct.Spirit.html#method.terminate)
    /// or when a termination signal is received.
    ///
    /// Note that there are ways the application may terminate without calling these hooks ‒ for
    /// example terminating the main thread, or aborting.
    ///
    /// TODO: Threads, deadlocks
    fn on_terminate<F>(self, hook: F) -> Self
    where
        F: FnMut() + Send + 'static;

    /// Add a closure run before the main body.
    ///
    /// The [`run`](#method.run) will first execute all closures submitted through this method
    /// before running the real body. They are run in the order of submissions.
    ///
    /// The purpose of this is mostly integration with helpers ‒ they often need some last minute
    /// preparation.
    ///
    /// In case of using only build, the bodies are composed into one object and returned as part
    /// of the result (the inner body).
    ///
    /// # Errors
    ///
    /// If any of the before-bodies in the chain return an error, the processing ends there and the
    /// error is returned right away.
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

    /// Wrap the body run by the [`run`](#method.run) into this closure.
    ///
    /// The inner body is passed as an object with a [`run`](struct.InnerBody.html#method.run)
    /// method, not as a closure, due to a limitation around boxed `FnOnce`.
    ///
    /// It is expected the wrapper executes the inner body as part of itself and propagates any
    /// returned error.
    ///
    /// In case of multiple wrappers, the ones submitted later on are placed inside the sooner
    /// ones ‒ the first one is the outermost.
    ///
    /// In case of using only [`build`](#method.build), all the wrappers composed together are
    /// returned as part of the result.
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

    /// Apply the first [`Helper`](helpers.trait.Helper.html) of the type.
    ///
    /// This applies the passed helper, but only if a helper with the same hasn't yet been applied
    /// (or the [`singleton`](#method.singleton) called manually).
    ///
    /// Note that different instances of the same type of a helper can act differently, but are
    /// still considered the same type. This means the first instance wins. This is considered a
    /// feature ‒ many other helpers need some environment to run in (like `tokio`). The helpers
    /// try to apply a default configuration, but the user can apply a specific configuration
    /// first.
    fn with_singleton<T>(self, singleton: T) -> Result<Self::Ok, Error>
    where
        T: Extension<Self::Ok> + 'static;
}

impl<C> Extensible for Result<C, Error>
where
    C: Extensible<Ok = C>,
{
    type Opts = C::Opts;
    type Config = C::Config;
    type Ok = C;
    const STARTED: bool = C::STARTED;

    fn config_validator<R, F>(self, f: F) -> Result<Self::Ok, Error>
    where
        F: FnMut(&Arc<Self::Config>, &mut Self::Config, &Self::Opts) -> R + Send + 'static,
        R: Into<ValidationResults>,
    {
        self.and_then(|c| c.config_validator(f))
    }

    fn on_config<F>(self, hook: F) -> Self
    where
        F: FnMut(&Self::Opts, &Arc<Self::Config>) + Send + 'static,
    {
        self.map(|c| c.on_config(hook))
    }

    fn on_signal<F>(self, signal: libc::c_int, hook: F) -> Result<Self::Ok, Error>
    where
        F: FnMut() + Send + 'static,
    {
        self.and_then(|c| c.on_signal(signal, hook))
    }

    fn on_terminate<F>(self, hook: F) -> Self
    where
        F: FnMut() + Send + 'static,
    {
        self.map(|c| c.on_terminate(hook))
    }

    fn run_before<B>(self, body: B) -> Result<Self::Ok, Error>
    where
        B: FnOnce(&Arc<Spirit<Self::Opts, Self::Config>>) -> Result<(), Error> + Send + 'static,
    {
        self.and_then(|c| c.run_before(body))
    }

    fn run_around<W>(self, wrapper: W) -> Result<Self::Ok, Error>
    where
        W: FnOnce(&Arc<Spirit<Self::Opts, Self::Config>>, InnerBody) -> Result<(), Error>
            + Send
            + 'static,
    {
        self.and_then(|c| c.run_around(wrapper))
    }

    fn with<E>(self, ext: E) -> Result<Self::Ok, Error>
    where
        E: Extension<Self::Ok>,
    {
        self.and_then(|c| c.with(ext))
    }

    fn singleton<T: 'static>(&mut self) -> bool {
        // If we are errored out, this doesn't really matter, but usually false means less work to
        // do.
        self.as_mut()
            .map(|c| c.singleton::<T>())
            .unwrap_or_default()
    }

    fn with_singleton<T>(self, singleton: T) -> Result<Self::Ok, Error>
    where
        T: Extension<Self::Ok> + 'static,
    {
        self.and_then(|c| c.with_singleton(singleton))
    }
}

/// The basic extension trait.
///
/// It allows being plugged into a builder and modifying it in an arbitrary way.
///
/// It is more common to apply the helper by the
/// [`Builder::with`](../struct.Builder.html#method.with) method than directly.
///
/// There's an implementation of `Extension` for `FnOnce(Builder) -> Result<Builder, Error>`, so
/// extensions can be either custom types or just closures (which are often more convenient than
/// defining an empty type and the implementation).
///
/// ```rust
/// use failure::Error;
/// use spirit::extension::Extension;
/// use spirit::prelude::*;
///
/// struct CfgPrint;
///
/// impl<E: Extensible> Extension<E> for CfgPrint {
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
/// fn cfg_print<E: Extensible>(ext: E) -> E {
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
    /// Perform the transformation on the given builder.
    ///
    /// And yes, it is possible to do multiple primitive transformations inside one helper (this is
    /// what makes helpers useful for 3rd party crates, they can integrate with just one call of
    /// [`with`](../struct.Builder.html#method.with)).
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

/// A specialized version of [`Helper`](trait.Helper.html) for a piece of extracted configuration.
///
/// This traits works in tandem with an extractor function and action. The extractor is supposed to
/// extract a specific piece of configuration. The trait is defined on the type returned by the
/// extractor and produces some kind of resource. The action is then performed with the resource.
///
/// As an example, the type implementing the trait could be a configuration for a TCP socket. The
/// extractor just pulls out the instance of the type out of the configuration. The action could be
/// whatever the application needs to do with the TCP socket. The helper then bridges these
/// together by making the socket out of the configuration.
///
/// The trait often delegates to the basic version of `Helper` under the hood, by connecting the
/// extractor with the „active“ part of the helper.
///
/// You can use the [`Builder::config_helper`](../struct.Builder.html#method.config_helper) to
/// apply a `CfgHelper`.
///
/// # TODO
///
/// This calls for an example.
///
/// # Future plans
///
/// It is planned to eventually have a custom derive for these kinds of helpers to compose a helper
/// of a bigger piece of configuration. The extractor would then be auto-generated.
pub trait CfgHelper<O, C, Action> {
    /// Perform the creation and application of the helper.
    ///
    /// # Params
    ///
    /// * `extractor`: Function that pulls out a bit of configuration out of the complete
    ///   configuration type.
    /// * `action`: Something application-specific performed with the resource built of the
    ///   relevant piece of configuration.
    /// * `name`: Named used in logs to reference the specific instance of the type in logs. It is
    ///   more useful to have „heartbeat connection“ instead of „tcp socket“ in there (often,
    ///   application has many different kinds of tcp sockets around).
    /// * `builder`: The builder to modify by this helper.
    fn apply<Extractor, Name>(
        extractor: Extractor,
        action: Action,
        name: Name,
        builder: Builder<O, C>,
    ) -> Builder<O, C>
    where
        Extractor: FnMut(&C) -> Self + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static;
}

/// A variant of the [`CfgHelper`](trait.CfgHelper.html) for resources that come in groups.
///
/// If an application should (for example) listen for incoming connections, it is often desirable
/// to be able to configure multiple listening endpoints at once.
///
/// In simple words, if the `IteratedCfgHelper` is implemented for a type, a `CfgHelper` is
/// implemented for a container of the type (eg. `Vec`). The extractor then extracts the vector and
/// the helper takes care of managing multiple instances of the resource.
///
/// # Single instance
///
/// If a helper is implemented in terms of `IteratedCfgHelper` and your application configuration
/// contains exactly one instance, it is possible to return `iter::once` from the extractor, which
/// will pretend the configuration contains a container of exactly one thing.
///
/// Some helper crates may already provide both implementations on the same type in this manner.
pub trait IteratedCfgHelper<O, C, Action> {
    /// Perform the transformation of the builder.
    ///
    /// It works the same way as [`CfgHelper::apply`](trait.CfgHelper.html#method.apply), only with
    /// slightly different types around the extractor.
    fn apply<Extractor, ExtractedIter, Name>(
        extractor: Extractor,
        action: Action,
        name: Name,
        builder: Builder<O, C>,
    ) -> Builder<O, C>
    where
        Self: Sized, // TODO: Why does rustc insist on this one?
        Extractor: FnMut(&C) -> ExtractedIter + Send + 'static,
        ExtractedIter: IntoIterator<Item = Self>,
        Name: Clone + Display + Send + Sync + 'static;
}

impl<O, C, Action, Iter, Target> CfgHelper<O, C, Action> for Iter
where
    Iter: IntoIterator<Item = Target>,
    Target: IteratedCfgHelper<O, C, Action>,
{
    fn apply<Extractor, Name>(
        extractor: Extractor,
        action: Action,
        name: Name,
        builder: Builder<O, C>,
    ) -> Builder<O, C>
    where
        Extractor: FnMut(&C) -> Self + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        <Target as IteratedCfgHelper<O, C, Action>>::apply(extractor, action, name, builder)
    }
}

/// A helper to store configuration to some global-ish storage.
///
/// This makes sure every time a new config is loaded, it is made available inside the passed
/// parameter. Therefore, places without direct access to the `Spirit` itself can look into the
/// configuration.
///
/// The parameter can be a lot of things, but usually:
///
/// * `Arc<ArcSwap<C>>`.
/// * A reference to global `ArcSwap<C>` (for example inside `lazy_static` or `once_cell`).
///
/// # Examples
///
/// ```rust
/// #[macro_use]
/// extern crate lazy_static;
/// extern crate spirit;
///
/// use std::sync::Arc;
///
/// use arc_swap::ArcSwap;
/// use spirit::prelude::*;
/// use spirit::extension;
///
/// lazy_static! {
///     static ref CFG: ArcSwap<Empty> = ArcSwap::from(Arc::new(Empty {}));
/// }
///
/// # fn main() {
/// # let _ =
/// Spirit::<Empty, Empty>::new()
///     // Will make sure CFG contains the newest config
///     .with(extension::cfg_store(&*CFG))
///     .build(false);
/// # }
/// ```
pub fn cfg_store<S, E>(storage: S) -> impl Extension<E>
where
    E: Extensible,
    S: Borrow<ArcSwap<E::Config>> + Send + Sync + 'static,
{
    |ext: E| ext.on_config(move |_o: &_, c: &Arc<E::Config>| storage.borrow().store(Arc::clone(c)))
}

/// A helper for one-time initial configuration.
///
/// Sometimes, some configuration values can't be reasonably updated at runtime (libraries don't
/// support reconfiguration, there's no time to do that, ...). This callback tries to improve the
/// situation around these configurations.
///
/// The `extractor` extracts a fragment of configuration every time a configuration is loaded. The
/// first time this happens, `init` is called with this extracted configuration. Upon any future
/// configuration reloads, a warning is issued (with the given `name`) if the configuration
/// contains a different value than the one it was originally initialized.
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

/// A helper to warn about changes to configuration that can't be updated at runtime.
///
/// This is similar to [`immutable_cfg_init`](fn.immutable_cfg_init.html), except that there's no
/// callback called at the first load.
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

impl<O, C> Builder<O, C>
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
{
    /// Apply a config helper to the builder.
    ///
    /// For more information see [`CfgHelper`](helpers/trait.CfgHelper.html).
    pub fn config_helper<Cfg, Extractor, Action, Name>(
        self,
        extractor: Extractor,
        action: Action,
        name: Name,
    ) -> Self
    where
        Extractor: FnMut(&C) -> Cfg + Send + 'static,
        Cfg: CfgHelper<O, C, Action>,
        Name: Clone + Display + Send + Sync + 'static,
    {
        trace!("Adding config helper for {}", name);
        CfgHelper::apply(extractor, action, name, self)
    }
}
