//! Helpers for configuration validation.
//!
//! See [`config_validator`](../struct.Builder.html#method.config_validator).
use std::fmt::{Display, Formatter, Result as FmtResult};

use failure::{Backtrace, Error, Fail};

/// An unknown-type validation error.
///
/// If the validation callback returns error with only a description, not a proper typed exception,
/// the description is turned into this if bundled into [`Error`](struct.Error.html).
#[derive(Debug, Fail)]
#[fail(display = "{}", _0)]
pub struct UntypedError(String);

/// An error caused by failed validation.
///
/// Carries all the errors that caused it to fail (publicly accessible). Multiple are possible.
///
/// The `cause` is delegated to the first error, if any is present.
#[derive(Debug)]
pub struct MultiError(pub Vec<Error>);

impl MultiError {
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

/// A validation result.
///
/// The validator (see [`config_validator`](../struct.Builder.html#method.config_validator)) is
/// supposed to return an arbitrary number of these results. Each one can hold a message (with
/// varying severity) and optionally a success and failure actions.
#[derive(Default)]
pub struct Action {
    pub(crate) on_abort: Option<Box<FnMut()>>,
    pub(crate) on_success: Option<Box<FnMut()>>,
}

impl Action {
    /// Creates actions without both hooks empty.
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
