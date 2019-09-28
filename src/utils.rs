//! Various utilities.
//!
//! All the little things that are useful through the spirit's or user's code, and don't really fit
//! anywhere else.

use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::fmt::{Debug, Display, Formatter, Result as FmtResult};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use log::warn;
use serde::de::{Deserializer, Error as DeError, Unexpected};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

use crate::AnyError;

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
///     #[structopt(short = "p", parse(from_os_str = spirit::utils::absolute_from_os_str))]
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
#[derive(Copy, Clone, Debug)]
pub struct MissingEquals;

impl Display for MissingEquals {
    fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
        write!(fmt, "Missing = in map option")
    }
}

impl Error for MissingEquals {}

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
///         parse(try_from_str = spirit::utils::key_val),
///         number_of_values(1),
///     )]
///     defines: Vec<(String, String)>,
/// }
///
/// # fn main() {}
/// ```
pub fn key_val<K, V>(opt: &str) -> Result<(K, V), AnyError>
where
    K: FromStr,
    K::Err: Error + Send + Sync + 'static,
    V: FromStr,
    V::Err: Error + Send + Sync + 'static,
{
    let pos = opt.find('=').ok_or(MissingEquals)?;
    Ok((opt[..pos].parse()?, opt[pos + 1..].parse()?))
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
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
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

/// Serialize a duration.
///
/// This can be used in configuration structures containing durations. The deserialization can be
/// done with serde-humantime.
///
/// The default serialization produces human unreadable values, this is more suitable for dumping
/// configuration users will read.
///
/// # Examples
///
/// ```rust
/// use std::time::Duration;
///
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
/// struct Cfg {
///     #[serde(
///         serialize_with = "spirit::utils::serialize_duration",
///         deserialize_with = "serde_humantime::deserialize",
///     )]
///     how_long: Duration,
/// }
/// ```
pub fn serialize_duration<S: Serializer>(dur: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&humantime::format_duration(*dur).to_string())
}

/// Deserialize an `Option<Duration>` using the [`humantime`] crate.
///
/// This allows reading human-friendly representations of time, like `30s` or `5days`. It should be
/// paired with [`serialize_opt_duration`]. Also, to act like [`Option`] does when deserializing by
/// default, the `#[serde(default)]` is recommended.
///
/// # Examples
///
/// ```rust
/// use std::time::Duration;
///
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
/// struct Cfg {
///     #[serde(
///         serialize_with = "spirit::utils::serialize_opt_duration",
///         deserialize_with = "spirit::utils::deserialize_opt_duration",
///         default,
///     )]
///     how_long: Option<Duration>,
/// }
/// ```
pub fn deserialize_opt_duration<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Duration>, D::Error> {
    if let Some(dur) = Option::<String>::deserialize(d)? {
        humantime::parse_duration(&dur)
            .map_err(|_| DeError::invalid_value(Unexpected::Str(&dur), &"Human readable duration"))
            .map(Some)
    } else {
        Ok(None)
    }
}

/// Serialize an `Option<Duration>` in a human friendly form.
///
/// See the [`deserialize_opt_duration`] for more details and an example.
pub fn serialize_opt_duration<S: Serializer>(
    dur: &Option<Duration>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match dur {
        Some(d) => serialize_duration(d, s),
        None => s.serialize_none(),
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
