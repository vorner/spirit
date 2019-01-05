#![doc(
    html_root_url = "https://docs.rs/spirit/0.2.10/spirit/",
    test(attr(deny(warnings)))
)]
#![allow(clippy::type_complexity)]
#![forbid(unsafe_code)]
//#![warn(missing_docs)]

//! A helper to create unix daemons.
//!
//! When writing a service (in the unix terminology, a daemon), there are two parts of the job. One
//! is the actual functionality of the service, the part that makes it different than all the other
//! services out there. And then there's the very boring part of turning the prototype
//! implementation into a well-behaved service and handling all the things expected of all of them.
//!
//! This crate is supposed to help with the latter. Before using, you should know the following:
//!
//! * This is an early version and while already (hopefully) useful, it is expected to expand and
//!   maybe change a bit in future versions. There are certainly parts of functionality I still
//!   haven't written and expect it to be rough around the edges.
//! * It is opinionated ‒ it comes with an idea about how a well-behaved daemon should look like
//!   and how to integrate that into your application. I simply haven't find a way to make it less
//!   opinionated yet and this helps to scratch my own itch, so it reflects what I needed. If you
//!   have use cases that you think should fall within the responsibilities of this crate and are
//!   not handled, you are of course welcome to open an issue (or even better, a pull request) on
//!   the repository ‒ it would be great if it scratched not only my own itch.
//! * It brings in a lot of dependencies. There will likely be features to turn off the unneeded
//!   parts, but for now, nobody made them yet.
//! * This supports unix-style daemons only *for now*. This is because I have no experience in how
//!   a service for different OS should look like. However, help in this area would be appreciated
//!   ‒ being able to write a single code and compile a cross-platform service with all the needed
//!   plumbing would indeed sound very useful.
//!
//! You can have a look at a [tutorial](https://vorner.github.io/2018/12/09/Spirit-Tutorial.html)
//! first before diving into the API documentation.
//!
//! # What the crate does and how
//!
//! To be honest, the crate doesn't bring much (or, maybe mostly none) of novelty functionality to
//! the table. It just takes other crates doing something useful and gluing them together to form
//! something most daemons want to do.
//!
//! By composing these things together the crate allows for cutting down on your own boilerplate
//! code around configuration handling, signal handling and command line arguments.
//!
//! Using the builder pattern, you create a singleton [`Spirit`] object. That one starts a
//! background thread that runs some callbacks configured previously when things happen.
//!
//! It takes two structs, one for command line arguments (using [`StructOpt`]) and another for
//! configuration (implementing [`serde`]'s [`Deserialize`], loaded using the [`config`] crate). It
//! enriches both to add common options, like configuration overrides on the command line and
//! logging into the configuration.
//!
//! The background thread listens to certain signals (like `SIGHUP`) using the [`signal-hook`] crate
//! and reloads the configuration when requested. It manages the logging backend to reopen on
//! `SIGHUP` and reflect changes to the configuration.
//!
//! [`Spirit`]: struct.Spirit.html
//! [`StructOpt`]: https://crates.io/crates/structopt
//! [`serde`]: https://crates.io/crates/serde
//! [`Deserialize`]: https://docs.rs/serde/*/serde/trait.Deserialize.html
//! [`config`]: https://crates.io/crates/config
//! [`signal-hook`]: https://crates.io/crates/signal-hook
//!
//! # Features
//!
//! There are several features that can tweak functionality. Currently, they are all *on* by
//! default, but they can be opted out of. All the other spirit crates depend only on the bare
//! minimum of features they need.
//!
//! * `ini`, `json`, `hjson`, `yaml`: support for given configuration formats.
//! * `cfg-help`: support for adding documentation to the configuration fragmtents that can be used
//!   by the [`spirit-cfg-helpers`] crate to add the `--help-config` command line option. It is
//!   implemented by the [`structdoc`] crate behind the scenes. On by default. This feature flag is
//!   actually available in all the other sub-crates too.
//!
//! # Helpers
//!
//! It brings the idea of helpers. A helper is something that plugs a certain functionality into
//! the main crate, to cut down on some more specific boiler-plate code. These are usually provided
//! by other crates. To list some:
//!
//! * [`spirit-cfg-helpers`]: Various helpers to provide `--config-help`, `--dump-config` and
//!   configuration debug logging.
//! * [`spirit-daemonize`]: Configuration and routines to go into background and be a nice daemon.
//! * [`spirit-log`]: Configuration of logging.
//! * [`spirit-tokio`]: Integrates basic tokio primitives ‒ auto-reconfiguration for TCP and UDP
//!   sockets and starting the runtime.
//! * [`spirit-reqwest`]: Configuration for the reqwest HTTP [`Client`][reqwest-client].
//! * [`spirit-hyper`]: Integrates the hyper web server.
//!
//! (Others will come over time)
//!
//! There are some general helpers in the [`helpers`](helpers/) module.
//!
//! # Examples
//!
//! ```rust
//! use std::time::Duration;
//! use std::thread;
//!
//! use log::{debug, info};
//! use serde::Deserialize;
//! use spirit::prelude::*;
//!
//! #[derive(Debug, Default, Deserialize)]
//! struct Cfg {
//!     message: String,
//!     sleep: u64,
//! }
//!
//! static DEFAULT_CFG: &str = r#"
//! message = "hello"
//! sleep = 2
//! "#;
//!
//! fn main() {
//!     Spirit::<Empty, Cfg>::new()
//!         // Provide default values for the configuration
//!         .config_defaults(DEFAULT_CFG)
//!         // If the program is passed a directory, load files with these extensions from there
//!         .config_exts(&["toml", "ini", "json"])
//!         .on_terminate(|| debug!("Asked to terminate"))
//!         .on_config(|_opts, cfg| debug!("New config loaded: {:?}", cfg))
//!         // Run the closure, logging the error nicely if it happens (note: no error happens
//!         // here)
//!         .run(|spirit: &_| {
//!             while !spirit.is_terminated() {
//!                 let cfg = spirit.config(); // Get a new version of config every round
//!                 thread::sleep(Duration::from_secs(cfg.sleep));
//!                 info!("{}", cfg.message);
//! #               spirit.terminate(); // Just to make sure the doc-test terminates
//!             }
//!             Ok(())
//!         });
//! }
//! ```
//!
//! More complete examples can be found in the
//! [repository](https://github.com/vorner/spirit/tree/master/examples).
//!
//! # Added configuration and options
//!
//! ## Command line options
//!
//! * `config-override`: Override configuration value.
//!
//! Furthermore, it takes a list of paths ‒ both files and directories. They are loaded as
//! configuration files (the directories are examined and files in them ‒ the ones passing a
//! [`filter`](struct.Builder.html#method.config_files) ‒ are also loaded).
//!
//! # Common patterns
//!
//! TODO
//!
//! [`spirit-cfg-helpers`]: https://crates.io/crates/spirit-cfg-helpers
//! [`spirit-daemonize`]: https://crates.io/crates/spirit-daemonize
//! [`spirit-log`]: https://crates.io/crates/spirit-log
//! [`spirit-tokio`]: https://crates.io/crates/spirit-tokio
//! [`spirit-reqwest`]: https://crates.io/crates/spirit-reqwest
//! [`spirit-hyper`]: https://crates.io/crates/spirit-hyper
//! [`reqwest-client`]: https://docs.rs/reqwest/~0.9.5/reqwest/struct.Client.html

pub mod app;
mod bodies;
pub mod cfg_loader;
mod empty;
pub mod extension;
pub mod fragment;
mod spirit;
pub mod utils;
pub mod validation;

pub use arc_swap::ArcSwap;

pub use crate::bodies::{InnerBody, WrapBody};
pub use crate::cfg_loader::ConfigBuilder;
pub use crate::empty::Empty;
pub use crate::extension::Extensible;
pub use crate::spirit::{Builder, Spirit, SpiritBuilder};
#[deprecated(note = "Moved to spirit::utils")]
pub use crate::utils::{key_val, log_error, log_errors, log_errors_named, MissingEquals};

pub mod prelude {
    pub use super::{ConfigBuilder, Empty, Extensible, Spirit, SpiritBuilder};
}
