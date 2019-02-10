//! Various utilities.
//!
//! All the little things that are useful through the spirit's or user's code, and don't really fit
//! anywhere else.

use std::env;
use std::ffi::OsStr;
use std::fmt::{Debug, Formatter, Result as FmtResult};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::str::FromStr;

use failure::{Error, Fail};
use itertools::Itertools;
use log::{debug, log, log_enabled, warn, Level};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

/// Tries to read an absolute path from the given OS string.
///
/// This converts the path to PathBuf. Then it tries to make it absolute and canonical, so changing
/// current directory later on doesn't make it invalid.
///
/// The function never fails. However, the substeps (finding current directory to make it absolute
/// and canonization) might fail. In such case, the failing step is skipped.
///
/// The motivation is parsing command line arguments using the [`structopt`] crate. Users are used
/// to passing relative paths to command line (as opposed to configuration files). However, if the
/// daemon changes the current directory (for example during daemonization), the relative paths now
/// point somewhere else.
///
/// # Examples
///
/// ```rust
/// use std::path::PathBuf;
///
/// use structopt::StructOpt;
///
/// #[derive(Debug, StructOpt)]
/// struct MyOpts {
///     #[structopt(short = "p", parse(from_os_str = "spirit::utils::absolute_from_os_str"))]
///     path: PathBuf,
/// }
///
/// # fn main() { }
/// ```
pub fn absolute_from_os_str(path: &OsStr) -> PathBuf {
    let mut current = env::current_dir().unwrap_or_else(|e| {
        warn!(
            "Some paths may not be turned to absolute. Couldn't read current dir: {}",
            e,
        );
        PathBuf::new()
    });
    current.push(path);
    if let Ok(canonicized) = current.canonicalize() {
        canonicized
    } else {
        current
    }
}

/// An error returned when the user passes a key-value option without the equal sign.
///
/// Some internal options take a key-value pairs on the command line. If such option is expected,
/// but it doesn't contain the equal sign, this is the used error.
#[derive(Debug, Fail)]
#[fail(display = "Missing = in map option")]
pub struct MissingEquals;

/// A helper for deserializing map-like command line arguments.
///
/// # Examples
///
/// ```rust
/// # use structopt::StructOpt;
/// #[derive(Debug, StructOpt)]
/// struct MyOpts {
///     #[structopt(
///         short = "D",
///         long = "define",
///         parse(try_from_str = "spirit::utils::key_val"),
///         raw(number_of_values = "1"),
///     )]
///     defines: Vec<(String, String)>,
/// }
///
/// # fn main() {}
/// ```
pub fn key_val<K, V>(opt: &str) -> Result<(K, V), Error>
where
    K: FromStr,
    K::Err: Fail + 'static,
    V: FromStr,
    V::Err: Fail + 'static,
{
    let pos = opt.find('=').ok_or(MissingEquals)?;
    Ok((opt[..pos].parse()?, opt[pos + 1..].parse()?))
}

/// How to format errors in logs.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ErrorLogFormat {
    /// Multi-cause error will span multiple log messages.
    Multiline,

    /// The error is formatted on a single line.
    ///
    /// The causes are separated by semicolons.
    ///
    /// If present, trace is printed on debug level.
    SingleLine,

    /// Like [SingleLine][ErrorLogFormat::SingleLine], but without the backtrace.
    SingleLineWithoutBacktrace,

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
/// [`log_errors`].
pub fn log_error(level: Level, target: &str, e: &Error, format: ErrorLogFormat) {
    // Note: one of the causes is the error itself
    match format {
        ErrorLogFormat::Multiline => {
            for cause in e.iter_chain() {
                log!(target: target, level, "{}", cause);
            }
        }
        ErrorLogFormat::SingleLine |
        ErrorLogFormat::SingleLineWithoutBacktrace => {
            log!(target: target, level, "{}", e.iter_chain().join("; "));
        }
        _ => unreachable!("Non-exhaustive sentinel should not be used"),
    }
    if log_enabled!(Level::Debug) && format != ErrorLogFormat::SingleLineWithoutBacktrace {
        let bt = format!("{}", e.backtrace());
        if !bt.is_empty() {
            debug!(target: target, "{}", bt);
        }
    }
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
/// # use failure::{Error, ResultExt};
/// # use spirit::utils;
/// # fn try_to_do_stuff() -> Result<(), Error> { Ok(()) }
///
/// let result = utils::log_errors(module_path!(), || {
///     try_to_do_stuff().context("Didn't manage to do stuff")?;
///     Ok(())
/// });
/// # let _result = result;
/// ```
pub fn log_errors<R, F>(target: &str, f: F) -> Result<R, Error>
where
    F: FnOnce() -> Result<R, Error>,
{
    let result = f();
    if let Err(ref e) = result {
        log_error(Level::Error, target, e, ErrorLogFormat::Multiline);
    }
    result
}

/// A wrapper to hide a configuration field from logs.
///
/// This acts in as much transparent way as possible towards the field inside. It only replaces the
/// [`Debug`] and [`Serialize`] implementations with returning `"******"`.
///
/// The idea is if the configuration contains passwords, they shouldn't leak into the logs.
/// Therefore, wrap them in this, eg:
///
/// ```rust
/// use std::io::Write;
/// use std::str;
///
/// use spirit::utils::Hidden;
///
/// #[derive(Debug)]
/// struct Cfg {
///     username: String,
///     password: Hidden<String>,
/// }
///
/// # fn main() -> Result<(), Box<std::error::Error>> {
/// let cfg = Cfg {
///     username: "me".to_owned(),
///     password: "secret".to_owned().into(),
/// };
///
/// let mut buffer: Vec<u8> = Vec::new();
/// write!(&mut buffer, "{:?}", cfg)?;
/// assert_eq!(r#"Cfg { username: "me", password: "******" }"#, str::from_utf8(&buffer)?);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[cfg_attr(feature = "cfg-help", derive(structdoc::StructDoc))]
#[repr(transparent)]
#[serde(transparent)]
pub struct Hidden<T>(pub T);

impl<T> From<T> for Hidden<T> {
    fn from(val: T) -> Self {
        Hidden(val)
    }
}

impl<T> Deref for Hidden<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for Hidden<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl<T> Debug for Hidden<T> {
    fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
        write!(fmt, "\"******\"")
    }
}

impl<T> Serialize for Hidden<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("******")
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::net::{AddrParseError, IpAddr};
    use std::num::ParseIntError;

    use super::*;

    #[test]
    fn abs() {
        let current = env::current_dir().unwrap();
        let parent = absolute_from_os_str(&OsString::from(".."));
        assert!(parent.is_absolute());
        assert!(current.starts_with(parent));

        let child = absolute_from_os_str(&OsString::from("this-likely-doesn't-exist"));
        assert!(child.is_absolute());
        assert!(child.starts_with(current));
    }

    /// Valid inputs for the key-value parser
    #[test]
    fn key_val_success() {
        assert_eq!(
            ("hello".to_owned(), "world".to_owned()),
            key_val("hello=world").unwrap()
        );
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        assert_eq!(("ip".to_owned(), ip), key_val("ip=192.0.2.1").unwrap());
        assert_eq!(("count".to_owned(), 4), key_val("count=4").unwrap());
    }

    /// The extra equals sign go into the value part.
    #[test]
    fn key_val_extra_equals() {
        assert_eq!(
            ("greeting".to_owned(), "hello=world".to_owned()),
            key_val("greeting=hello=world").unwrap(),
        );
    }

    /// Test when the key or value doesn't get parsed.
    #[test]
    fn key_val_parse_fail() {
        key_val::<String, IpAddr>("hello=192.0.2.1.0")
            .unwrap_err()
            .downcast_ref::<AddrParseError>()
            .expect("Different error returned");
        key_val::<usize, String>("hello=world")
            .unwrap_err()
            .downcast_ref::<ParseIntError>()
            .expect("Different error returned");
    }

    #[test]
    fn key_val_missing_eq() {
        key_val::<String, String>("no equal sign")
            .unwrap_err()
            .downcast_ref::<MissingEquals>()
            .expect("Different error returned");
    }
}
