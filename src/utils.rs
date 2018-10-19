//! Various utilities.
//!
//! All the little things that are useful through the spirits code, can be useful to users and
//! don't logically fit anywhere else.

use std::env;
use std::ffi::OsStr;
use std::path::PathBuf;

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

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

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
}
