//! Error handling utilities.

use std::error::Error;

use err_context::prelude::*;
use log::{log, Level};

/// A wrapper type for any error.
///
/// This is just a type alias for boxed standard error. Any errors go and this is guaranteed to be
/// fully compatible.
pub type AnyError = Box<dyn Error + Send + Sync>;

/// How to format errors in logs.
///
/// The enum is non-exhaustive ‒ more variants may be added in the future and it won't be
/// considered an API breaking change.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ErrorLogFormat {
    /// Multi-cause error will span multiple log messages.
    MultiLine,

    /// The error is formatted on a single line.
    ///
    /// The causes are separated by semicolons.
    SingleLine,

    // Prevent users from accidentally matching against this enum without a catch-all branch.
    #[doc(hidden)]
    #[allow(non_camel_case_types)]
    _NON_EXHAUSTIVE,
}

/// Log one error on given log level.
///
/// It is printed to the log with all the causes and optionally a backtrace (if it is available and
/// debug logging is enabled).
///
/// This is the low-level version with full customization. You might also be interested in
/// [`log_errors`] or one of the convenience macro ([`log_error`][macro@log_error]).
pub fn log_error(level: Level, target: &str, e: &AnyError, format: ErrorLogFormat) {
    match format {
        ErrorLogFormat::MultiLine => {
            for cause in e.chain() {
                log!(target: target, level, "{}", cause);
            }
        }
        ErrorLogFormat::SingleLine => {
            log!(target: target, level, "{}", e.display("; "));
        }
        _ => unreachable!("Non-exhaustive sentinel should not be used"),
    }
}

/// A convenience macro to log an [`AnyError`].
///
/// This logs an [`AnyError`] on given log level as a single line without backtrace. Removes some
/// boilerplate from the [`log_error`] function.
///
/// # Examples
///
/// ```rust
/// use std::error::Error;
/// use std::fmt::{Display, Formatter, Result as FmtResult};
/// use spirit::log_error;
///
/// #[derive(Debug)]
/// struct Broken;
///
/// impl Display for Broken {
///     fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
///         write!(fmt, "Something is broken")
///     }
/// }
///
/// impl Error for Broken {}
///
/// log_error!(Warn, Broken.into());
/// ```
///
/// [`log_error`]: fn@crate::error::log_error
#[macro_export]
macro_rules! log_error {
    ($level: ident, $descr: expr => $err: expr) => {
        $crate::log_error!(@SingleLine, $level, $err.context($descr).into());
    };
    ($level: ident, $err: expr) => {
        $crate::log_error!(@SingleLine, $level, $err);
    };
    (multi $level: ident, $descr: expr => $err: expr) => {
        $crate::log_error!(@MultiLine, $level, $err.context($descr).into());
    };
    (multi $level: ident, $err: expr) => {
        $crate::log_error!(@MultiLine, $level, $err);
    };
    (@$format: ident, $level: ident, $err: expr) => {
        $crate::error::log_error(
            $crate::macro_support::Level::$level,
            module_path!(),
            &$err,
            $crate::error::ErrorLogFormat::$format,
        );
    };
}

/// A wrapper around a fallible function, logging any returned errors.
///
/// The errors will be logged in the provided target. You may want to provide `module_path!` as the
/// target.
///
/// If the error has multiple levels (causes), they are printed in multi-line fashion, as multiple
/// separate log messages.
///
/// # Examples
///
/// ```rust
/// use err_context::prelude::*;
/// use spirit::AnyError;
/// use spirit::error;
/// # fn try_to_do_stuff() -> Result<(), AnyError> { Ok(()) }
///
/// let result = error::log_errors(module_path!(), || {
///     try_to_do_stuff().context("Didn't manage to do stuff")?;
///     Ok(())
/// });
/// # let _result = result;
/// ```
pub fn log_errors<R, F>(target: &str, f: F) -> Result<R, AnyError>
where
    F: FnOnce() -> Result<R, AnyError>,
{
    let result = f();
    if let Err(ref e) = result {
        log_error(Level::Error, target, e, ErrorLogFormat::MultiLine);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::fmt::{Display, Formatter, Result as FmtResult};

    use super::*;

    #[derive(Copy, Clone, Debug)]
    struct Dummy;

    impl Display for Dummy {
        fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
            write!(fmt, "Dummy error")
        }
    }

    impl Error for Dummy {}

    #[test]
    fn log_error_macro() {
        let err = Dummy;
        log_error!(Debug, err.into());
        log_error!(Debug, &err.into());
        log_error!(Debug, err.context("Another level").into());
        log_error!(Debug, "Another level" => err);
        let multi_err = err.context("Another level").into();
        log_error!(multi Info, multi_err);
    }
}
