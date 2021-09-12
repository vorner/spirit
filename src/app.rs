//! The running application part.
//!
//! The convenient way to manage the application runtime is through
//! [`Builder::run`][crate::SpiritBuilder::run]. If more flexibility is needed, the
//! [`Builder::build`][crate::SpiritBuilder::build] can be used instead. That method returns the
//! [`App`][crate::app::App] object, representing the application runner. The application can then
//! be run at any later time, as convenient.

use std::process;
use std::sync::Arc;
use std::thread;

use log::debug;
use serde::de::DeserializeOwned;
use structopt::StructOpt;

use crate::bodies::{InnerBody, WrapBody};
use crate::error;
use crate::spirit::Spirit;
use crate::terminate_guard::TerminateGuard;
use crate::utils::FlushGuard;
use crate::AnyError;

/// The running application part.
///
/// This is returned by [`Builder::build`][crate::SpiritBuilder::build] and represents the rest of
/// the application runtime except the actual application body. It can be used to run at any later
/// time, after the spirit has been created.
///
/// This carries all the [around-bodies][crate::Extensible::run_around] and
/// [before-bodies][crate::Extensible::run_before]. If you run the application body directly, not
/// through this, some of the pipelines or extensions might not work as expected.
///
/// The [`Builder::run`][crate::SpiritBuilder::run] is just a convenient wrapper around this. Note
/// that that one handles and logs errors from the application startup as well as from its runtime.
/// Here it is up to the caller to handle the startup errors.
///
/// # Examples
///
/// ```rust
/// use spirit::{AnyError, Empty, Spirit};
/// use spirit::prelude::*;
///
/// # fn main() -> Result<(), AnyError> {
/// Spirit::<Empty, Empty>::new()
///     .build(true)?
///     .run_term(|| {
///         println!("Hello world");
///         Ok(())
///     });
/// # Ok(())
/// # }
/// ```
pub struct App<O, C> {
    spirit: Arc<Spirit<O, C>>,
    inner: InnerBody,
    wrapper: WrapBody,
}

impl<O, C> App<O, C>
where
    O: StructOpt + Send + Sync + 'static,
    C: DeserializeOwned + Send + Sync + 'static,
{
    pub(crate) fn new(spirit: Arc<Spirit<O, C>>, inner: InnerBody, wrapper: WrapBody) -> Self {
        Self {
            spirit,
            inner,
            wrapper,
        }
    }

    /// Access to the built spirit object.
    ///
    /// The object can be used to manipulate the runtime of the application, access the current
    /// configuration and register further callbacks (and extensions and pipelines).
    ///
    /// Depending on your needs, you may pass it to the closure started with [`run`][App::run] or
    /// even placed into some kind of global storage.
    pub fn spirit(&self) -> &Arc<Spirit<O, C>> {
        &self.spirit
    }

    /// Run the application with provided body.
    ///
    /// This will run the provided body. However, it'll wrap it in all the
    /// [around-bodies][crate::Extensible::run_around] and precede it with all the
    /// [before-bodies][crate::Extensible::run_before]. If any of these fail, or if the `body`
    /// fails, the error is propagated (and further bodies are not started).
    ///
    /// Furthermore, depending on the [`autojoin_bg_thread`][crate::Extensible::autojoin_bg_thread]
    /// configuration, termination and joining of the background thread may be performed. If the
    /// body errors, termination is done unconditionally (which may be needed in some corner cases
    /// to not deadlock on error).
    ///
    /// In other words, unless you have very special needs, this is how you actually invoke the
    /// application itself.
    ///
    /// Any errors are simply returned and it is up to the caller to handle them somehow.
    pub fn run<B>(self, body: B) -> Result<(), AnyError>
    where
        B: FnOnce() -> Result<(), AnyError> + Send + 'static,
    {
        debug!("Running bodies");
        let _flush = FlushGuard;
        struct ScopeGuard<F: FnOnce()>(Option<F>);
        impl<F: FnOnce()> Drop for ScopeGuard<F> {
            fn drop(&mut self) {
                self.0.take().expect("Drop called twice")();
            }
        }
        let spirit = &self.spirit;
        let _thread = ScopeGuard(Some(|| {
            if thread::panicking() {
                spirit.terminate();
            }
            spirit.maybe_autojoin_bg_thread();
        }));
        let inner = self.inner;
        let inner = move || inner().and_then(|()| body());
        let result = (self.wrapper)(Box::new(inner));
        if result.is_err() {
            self.spirit.terminate();
        }
        result
    }

    /// Similar to [`run`][App::run], but with error handling.
    ///
    /// This calls the [`run`][App::run]. However, if there are any errors, they are logged and the
    /// application terminates with non-zero exit code.
    pub fn run_term<B>(self, body: B)
    where
        B: FnOnce() -> Result<(), AnyError> + Send + 'static,
    {
        let flush = FlushGuard;
        if error::log_errors("top-level", || self.run(body)).is_err() {
            drop(flush);
            process::exit(1);
        }
    }

    /// Run the application in a background thread for testing purposes.
    ///
    /// This'll run the application and return an RAII guard. That guard can be used to access the
    /// [Spirit] and manipulate it. It also terminates the application and background thread when
    /// dropped.
    ///
    /// This is for testing purposes (it panics if there are errors). See the [testing guide].
    ///
    /// testing guide: crate::guide::testing
    pub fn run_test<B>(self, body: B) -> TerminateGuard<O, C>
    where
        B: FnOnce() -> Result<(), AnyError> + Send + 'static,
    {
        let spirit = Arc::clone(self.spirit());
        let bg_thread = thread::spawn(move || self.run(body));
        TerminateGuard::new(spirit, bg_thread)
    }
}
