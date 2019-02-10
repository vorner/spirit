//! Helpers for configuration validation.
//!
//! See [`config_validator`][crate::Extensible::config_validator].
use std::fmt::{Display, Formatter, Result as FmtResult};

use failure::{Backtrace, Error, Fail};

/// An error caused by failed validation.
///
/// Carries all the errors that caused it to fail (publicly accessible). Multiple are possible.
///
/// The `cause` is delegated to the first error, if any is present.
#[derive(Debug)]
pub struct MultiError(pub Vec<Error>);

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
    pub fn wrap(mut errs: Vec<Error>) -> Error {
        match errs.len() {
            0 => panic!("No errors in multi-error"),
            1 => errs.pop().unwrap(),
            _ => MultiError(errs).into(),
        }
    }
}

impl Display for MultiError {
    fn fmt(&self, formatter: &mut Formatter) -> FmtResult {
        write!(
            formatter,
            "Config validation failed with {} errors",
            self.0.len()
        )
    }
}

impl Fail for MultiError {
    // There may actually be multiple causes. But we just stick with the first one for lack of
    // better way to pick.
    fn cause(&self) -> Option<&dyn Fail> {
        self.0.get(0).map(Error::as_fail)
    }

    fn backtrace(&self) -> Option<&Backtrace> {
        self.0.get(0).map(Error::backtrace)
    }
}

/// A validation action.
///
/// The validator (see [`config_validator`][crate::Extensible::config_validator]) is
/// supposed to either return an error or an action to be taken once validation completes.
///
/// By default, the [`Action`] is empty, but an [`on_success`][Action::on_success] and
/// [`on_abort`][Action::on_abort] callbacks can be attached to it. These'll execute once the
/// validation completes (only one of them will be called, depending on the result of validation).
///
/// # Examples
///
/// ```rust
/// use spirit::prelude::*;
/// use spirit::validation::Action;
/// # fn create_something<T>(_cfg: T) -> Result<Empty, failure::Error> { Ok(Empty {}) }
/// # fn install_something(_empty: Empty) {}
/// # let _ =
/// Spirit::<Empty, Empty>::new()
///     .config_validator(|_old_cfg, new_cfg, _opts| {
///         let something = create_something(new_cfg)?;
///         Ok(Action::new().on_success(move || install_something(something)))
///     });
/// ```
///
/// Or, if you want to only check the configuration:
///
/// ```rust
/// use failure::ensure;
/// use spirit::prelude::*;
/// use spirit::validation::Action;
/// # fn looks_good<T>(_cfg: T) -> bool { true }
/// # let _ =
/// Spirit::<Empty, Empty>::new()
///     .config_validator(|_old_cfg, new_cfg, _opts| {
///         ensure!(looks_good(new_cfg), "Configuration is broken");
///         Ok(Action::new())
///     });
/// ```
#[derive(Default)]
pub struct Action {
    pub(crate) on_abort: Option<Box<FnMut()>>,
    pub(crate) on_success: Option<Box<FnMut()>>,
}

impl Action {
    /// Creates actions wit both hooks empty.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches (replaces) the success action.
    pub fn on_success<F: FnOnce() + 'static>(self, f: F) -> Self {
        let mut f = Some(f);
        let wrapper = move || (f.take().unwrap())();
        Self {
            on_success: Some(Box::new(wrapper)),
            ..self
        }
    }

    /// Attaches (replaces) the failure action.
    pub fn on_abort<F: FnOnce() + 'static>(self, f: F) -> Self {
        let mut f = Some(f);
        let wrapper = move || (f.take().unwrap())();
        Self {
            on_abort: Some(Box::new(wrapper)),
            ..self
        }
    }

    pub(crate) fn run(self, success: bool) {
        let selected = if success {
            self.on_success
        } else {
            self.on_abort
        };
        if let Some(mut cback) = selected {
            cback();
        }
    }
}
