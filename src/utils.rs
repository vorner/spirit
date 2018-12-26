//! Various utilities.
//!
//! All the little things that are useful through the spirits code, can be useful to users and
//! don't logically fit anywhere else.

use std::env;
use std::ffi::OsStr;
use std::fmt::{Debug, Formatter, Result as FmtResult};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::str::FromStr;

use failure::{Error, Fail};
use log::Level;
use serde::ser::{Serialize, Serializer};

/// Tries to read an absolute path from the given OS string.
///
/// This converts the path to PathBuf. Then it tries to make it absolute and canonical, so changing
/// current directory later on doesn't make it invalid.
///
/// The function never fails. However, the substeps (finding current directory to make it absolute
/// and canonization) might fail. In such case, the failing step is skipped.
///
/// The motivation is parsing command line arguments using the `structopt` crate. Users are used to
/// passing relative paths to command line (as opposed to configuration files). However, if the
/// daemon changes the current directory (for example during daemonization), the relative paths now
/// point somewhere else.
///
/// # Examples
///
/// ```rust
/// extern crate spirit;
/// #[macro_use]
/// extern crate structopt;
///
/// use std::path::PathBuf;
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

/// An error returned when the user passes a key-value option without equal sign.
///
/// Some internal options take a key-value pairs on the command line. If such option is expected,
/// but it doesn't contain the equal sign, this is the used error.
#[derive(Debug, Fail)]
#[fail(display = "Missing = in map option")]
pub struct MissingEquals;

/// A helper for deserializing map-like command line arguments.
// TODO: In some future version, move to utils
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

/// Log one error
///
/// It is printed to the log with all the causes and optionally a backtrace (if it is available and
/// debug logging is enabled).
// TODO: Provide a macro, so user doesn't have to provide the target matually
pub fn log_error(target: &str, e: &Error) {
    // Note: one of the causes is the error itself
    for cause in e.iter_chain() {
        error!(target: target, "{}", cause);
    }
    if log_enabled!(Level::Debug) {
        let bt = format!("{}", e.backtrace());
        if !bt.is_empty() {
            debug!(target: target, "{}", bt);
        }
    }
}

/// Same as [`log_errors`](fn.log_errors), but with an explicit `target` to log into.
pub fn log_errors_named<R, F>(target: &str, f: F) -> Result<R, Error>
where
    F: FnOnce() -> Result<R, Error>,
{
    let result = f();
    if let Err(ref e) = result {
        log_error(target, e);
    }
    result
}

/// A wrapper around a fallible function that logs any returned errors, with all the causes and
/// optionally the backtrace.
pub fn log_errors<R, F: FnOnce() -> Result<R, Error>>(f: F) -> Result<R, Error> {
    log_errors_named("spirit", f)
}

/// A wrapper to hide a configuration field from logs.
///
/// This acts in as much transparent way as possible towards the field inside. It only replaces the
/// [`Debug`] implementation with returning `"******"`.
///
/// The idea is if the configuration contains passwords, they shouldn't leak into the logs.
/// Therefore, wrap them in this, eg:
///
/// ```rust
/// # extern crate spirit;
/// #
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
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
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
