//! Scaling of resources.
//!
//! As described with the [`ResourceConfig`] trait, resources can be scaled into multiple
//! instances. The items in this module specify how to configure the number of instances.

use std::fmt::Display;

use spirit::validation::{Result as ValidationResult, Results as ValidationResults};
use spirit::Empty;

fn default_scale() -> usize {
    1
}

/// Description of scaling into multiple tasks.
///
/// It is possible to create multiple instances of a resource, to scale it up for performance and
/// to share the load across multiple threads in the threadpool tokio executore.
///
/// Note that in case of TCP, even if there's just one instance of the listening sockets, the
/// individual connections still can be handled by multiple threads, as each accepted connection is
/// a separate task. However, if you use UDP or have too many (lightweight) connections to saturate
/// the single listening instance, having more than one can help.
///
/// While it is possible to define on `Scaled` types, it is expected the provided ones should be
/// enough.
///
/// # The empty scale
///
/// Using [`Empty`] as the scaling fragment will always return 1 instance and won't add any
/// configuration.
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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(StructDoc))]
pub struct Scale {
    /// Start this many listener instances.
    ///
    /// The listeners are internal only, but can be used to boost performance by accepting multiple
    /// connections in parallel if configured to more than 1.
    ///
    /// Defaults to 1.
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

impl Scaled for Empty {
    fn scaled<Name: Display>(&self, _: Name) -> (usize, ValidationResults) {
        (1, ValidationResults::new())
    }
}
