//! Reexports and support for our macros.
//!
//! This module contains reexports of some foreign types so macros can access them through the
//! $crate path. However, the module is not considered part of public API for the sake of semver
//! guarantees and its contents is not to be used directly in user code.

pub use log::Level;
