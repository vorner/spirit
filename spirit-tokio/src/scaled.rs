use std::fmt::Display;

use spirit::validation::{Result as ValidationResult, Results as ValidationResults};

fn default_scale() -> usize {
    1
}

/// Description of scaling into multiple tasks.
///
/// The helpers in this crate allow creating multiple copies of the socket. If using the default
/// (threadpool) tokio executor, it makes it possible to share the load across multiple threads.
///
/// Note that in case of TCP, even if there's just one instance of the listening sockets, the
/// individual connections still can be handled by multiple threads, as each accepted connection is
/// a separate task. However, if you use UDP or have too many (lightweight) connections to saturate
/// the single listening instance, having more than one can help.
///
/// While it is possible to define on `Scaled` types, it is expected the provided ones should be
/// enough.
pub trait Scaled {
    /// Returns how many instances there should be.
    ///
    /// And accompanies it with optional validation results, to either refuse or warn about the
    /// configuration.
    fn scaled<Name: Display>(&self, name: Name) -> (usize, ValidationResults);
}

/// A scaling configuration provided by user.
///
/// This contains a single option `scale` (which is embedded inside the relevant configuration
/// section), specifying the number of parallel instances. If the configuration is not provided,
/// this contains `1`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Scale {
    #[serde(default = "default_scale")]
    scale: usize,
}

impl Default for Scale {
    fn default() -> Self {
        Scale { scale: 1 }
    }
}

impl Scaled for Scale {
    fn scaled<Name: Display>(&self, name: Name) -> (usize, ValidationResults) {
        if self.scale > 0 {
            (self.scale, ValidationResults::new())
        } else {
            let msg = format!("Turning scale in {} from 0 to 1", name);
            (1, ValidationResult::warning(msg).into())
        }
    }
}

/// Turns scaling off and provides a single instance.
///
/// This contains no configuration options and work just as a „plug“ type to fill in a type
/// parameter.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Singleton {}

impl Scaled for Singleton {
    fn scaled<Name: Display>(&self, _: Name) -> (usize, ValidationResults) {
        (1, ValidationResults::new())
    }
}
