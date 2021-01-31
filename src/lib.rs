#![doc(test(attr(deny(warnings))))]
#![allow(
    unknown_lints,
    renamed_and_removed_lints,
    clippy::unknown_clippy_lints,
    clippy::type_complexity,
    clippy::needless_doctest_main
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

//! Helpers to cut down on boilerplate when writing services.
//!
//! When writing a service (in the unix terminology, a daemon), there are two parts of the job. One
//! is the actual functionality of the service, the part that makes it different than all the other
//! services out there ‒ in other words, the interesting part. And then there's the very boring
//! part of turning the prototype implementation into a well-behaved service with configuration,
//! logging, metrics, signal handling and whatever else one considers to be part of the deal.
//!
//! This crate is supposed to help with the latter. Surely, there's still something left to do but
//! the aim is to provide reusable building blocks to get the boring stuff done as fast as minimal
//! fuss as possible.
//!
//! # Foreword
//!
//! Before using this crate (or, family of crates), you should know few things:
//!
//! * While there has been some experimentation how the API should look like and it is being used
//!   in production software, the API is probably not final. One one hand that means upgrading to
//!   next version might need some work. On the other hand, if it doesn't fit your needs or use
//!   case, this is a great time to discuss it now, it might be possible to amend it and make it do
//!   what you need in the next version.
//! * Also, there's a lot to be done still ‒ both in terms of documentation, tutorials, examples
//!   but missing functionality as well (eg. fragments for configuring more things). Help in that
//!   direction is welcome ‒ if you find yourself in the need to configure something and have to
//!   roll your own implementation, consider sharing it with others.
//! * It is being tested on unix, with unix-style daemons. Supporting Windows should be possible in
//!   theory, but I don't have it at hand. If you use Windows, consider trying it out and adding
//!   support.
//! * The crate is on the heavier spectrum when it comes to dependencies, with aim for
//!   functionality and ease of use. Think more about server-side or desktop services. While it is
//!   possible to cut down on them somewhat by tweaking the feature flags, you probably don't want
//!   to use this in embedded scenarios.
//! * The crate doesn't come with much original functionality. Mostly, it is a lot of other great
//!   crates glued together to create a cohesive whole. That means you can do most of the stuff
//!   without spirit (though the crates come with few little utilities or tiny workarounds for
//!   problems you would face if you started to glue the things together).
//!
//! # The user guide
//!
//! You will find the API documentation here, but we also have the [user guide][crate::guide],
//! including a [tutorial][crate::guide::tutorial]. It might help you to get up to speed to read
//! through it, to know about the principles of how it works.
//!
//! # Features
//!
//! There are several features that can tweak functionality. Currently, the `json`, `yaml` and
//! `cfg-help` are on by default. All the other spirit crates depend only on the bare minimum of
//! features they need (and may have their own features).
//!
//! * `ini`, `json`, `hjson`, `yaml`: support for given configuration formats.
//! * `cfg-help`: support for adding documentation to the configuration fragmtents that can be used
//!   by the [`spirit-cfg-helpers`] crate to add the `--help-config` command line option. It is
//!   implemented by the [`structdoc`] crate behind the scenes. On by default. This feature flag is
//!   actually available in all the other sub-crates too.
//! * `color`: support for colored command line help (on by default).
//! * `suggestions`: support for command line suggestions on errors (on by default).
//!
//! [`Spirit`]: crate::Spirit
//! [`Builder`]: crate::Builder
//! [`AnyError`]: crate::AnyError
//! [`log_error`]: macro@crate::log_error
//! [`serde`]: https://lib.rs/crates/serde
//! [`Deserialize`]: https://docs.rs/serde/*/serde/trait.Deserialize.html
//! [`config`]: https://lib.rs/crates/config
//! [`signal-hook`]: https://lib.rs/crates/signal-hook
//! [`spirit-cfg-helpers`]: https://lib.rs/crates/spirit-cfg-helpers
//! [`spirit-daemonize`]: https://lib.rs/crates/spirit-daemonize
//! [`spirit-log`]: https://lib.rs/crates/spirit-log
//! [`spirit-tokio`]: https://lib.rs/crates/spirit-tokio
//! [`spirit-reqwest`]: https://lib.rs/crates/spirit-reqwest
//! [`spirit-hyper`]: https://lib.rs/crates/spirit-hyper
//! [`spirit-dipstick`]: https://lib.rs/crates/spirit-dipstick
//! [reqwest-client]: https://docs.rs/reqwest/~0.9.5/reqwest/struct.Client.html
//! [repository]: https://github.com/vorner/spirit
//! [tutorial]: https://vorner.github.io/2018/12/09/Spirit-Tutorial.html
//! [`err-context`]: https://lib.rs/crates/err-context
//! [`failure`]: https://lib.rs/crates/failure
//! [`err-context`]: https://lib.rs/crates/err-context
//! [`err-derive`]: https://lib.rs/crates/err-derive

pub mod app;
mod bodies;
pub mod cfg_loader;
mod empty;
pub mod error;
pub mod extension;
pub mod fragment;
pub mod guide;
#[doc(hidden)]
pub mod macro_support;
mod spirit;
pub mod utils;
pub mod validation;

pub use crate::cfg_loader::ConfigBuilder;
pub use crate::empty::Empty;
pub use crate::error::AnyError;
pub use crate::extension::Extensible;
pub use crate::fragment::pipeline::Pipeline;
pub use crate::fragment::Fragment;
pub use crate::spirit::{Builder, Spirit, SpiritBuilder};

/// The prelude.
///
/// To use the spirit libraries effectively, a lot of traits need to be imported. Instead
/// of importing them one by one manually, the [`prelude`][crate::prelude] contains the most
/// commonly used imports that are used around application runtime management. This imports only
/// traits and only in anonymous mode (eg. `pub use spirit::SpiritBuilder as _`).
///
/// This can be imported as `use spirit::prelude::*`.
pub mod prelude {
    pub use super::{ConfigBuilder as _, Extensible as _, Fragment as _, SpiritBuilder as _};
}
