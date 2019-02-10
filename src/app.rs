//! The running application part.
//!
//! The convenient way to manage the application runtime is through
//! [`Builder::run`][crate::SpiritBuilder::run]. If more flexibility is needed, the
//! [`Builder::build`][crate::SpiritBuilder::build] can be used instead. That method returns the
//! [`App`][crate::app::App] object, representing the application runner. The application can then
//! be run at any later time, as convenient.
use std::process;
use std::sync::Arc;

use failure::Error;
use log::debug;

use crate::bodies::{InnerBody, WrapBody};
use crate::spirit::Spirit;
use crate::utils;

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
/// use spirit::prelude::*;
///
/// # fn main() -> Result<(), failure::Error> {
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
    O: Send + Sync + 'static,
    C: Send + Sync + 'static,
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
    /// Any errors are simply returned and it is up to the caller to handle them somehow.
    pub fn run<B>(self, body: B) -> Result<(), Error>
    where
        B: FnOnce() -> Result<(), Error> + Send + 'static,
    {
        debug!("Running bodies");
        let inner = self.inner;
        let inner = move || inner.run().and_then(|()| body());
        self.wrapper.run(InnerBody(Box::new(Some(|()| inner()))))
    }

    /// Similar to [`run`][App::run], but with error handling.
    ///
    /// This calls the [`run`][App::run]. However, if there are any errors, they are logged and the
    /// application terminates with non-zero exit code.
    pub fn run_term<B>(self, body: B)
    where
        B: FnOnce() -> Result<(), Error> + Send + 'static,
    {
        if utils::log_errors("top-level", || self.run(body)).is_err() {
            process::exit(1);
        }
    }
}
