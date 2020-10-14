#![doc(
    html_root_url = "https://docs.rs/spirit-tokio/0.6.1/",
    test(attr(deny(warnings)))
)]
// Our program-long snippets are more readable with main
#![allow(clippy::needless_doctest_main)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

//! Support for tokio inside spirit
//!
//! This provides configuration of the tokio runtime and installer of futures.
//!
//! It also provides few configuration [`Fragment`]s for configuring network primitives.
//!
//! Note that this enables several features of [`tokio`].
//!
//! # Features
//!
//! * `rt-from-cfg`: Allows creating runtime from configuration. Enables the [`Tokio::FromCfg`]
//!   variant and [`Cfg`][crate::runtime::Cfg]. Enabled by default.
//! * `cfg-help`: Support for generating help for the configuration options.
//! * `net`: Network primitive configuration [`Fragment`]s in the [`net`] module.
//! * `stream`: Implementations of [`tokio::stream::Stream`] on several types.
//! * `futures`: Support for converting between [`futures`][futures_util]' and ours
//!   [`Either`][crate::either::Either].
//! * `either`: Support for converting between our [`Either`][crate::either::Either] and the one
//!   from the [`either`] crate.
//!
//! # Examples
//!
//! ```rust
//! use std::future::Future;
//! use std::pin::Pin;
//! use std::time::Duration;
//!
//! use err_context::AnyError;
//! use serde::{Deserialize, Serialize};
//! use spirit::{Empty, Pipeline, Spirit};
//! use spirit::prelude::*;
//! use spirit::fragment::driver::CacheEq;
//! use spirit_tokio::{FutureInstaller, Tokio};
//! use spirit_tokio::runtime::Config as TokioCfg;
//! use structdoc::StructDoc;
//!
//! #[derive(Clone, Debug, Deserialize, PartialEq, Serialize, StructDoc)]
//! #[serde(default)]
//! struct MsgCfg {
//!     /// A message to print now and then.
//!     msg: String,
//!     /// Time between printing the message.
//!     interval: Duration,
//! }
//!
//! impl MsgCfg {
//!     async fn run(self) {
//!         loop {
//!             println!("{}", self.msg);
//!             tokio::time::delay_for(self.interval).await;
//!         }
//!     }
//! }
//!
//! impl Default for MsgCfg {
//!     fn default() -> Self {
//!         MsgCfg {
//!             msg: "Hello".to_owned(),
//!             interval: Duration::from_secs(1),
//!         }
//!     }
//! }
//!
//! spirit::simple_fragment! {
//!     impl Fragment for MsgCfg {
//!         type Driver = CacheEq<MsgCfg>;
//!         type Resource = Pin<Box<dyn Future<Output = ()> + Send>>;
//!         type Installer = FutureInstaller;
//!         fn create(&self, _: &'static str) -> Result<Self::Resource, AnyError> {
//!             let fut = self.clone().run();
//!             Ok(Box::pin(fut))
//!         }
//!     }
//! }
//!
//! /// An application.
//! #[derive(Default, Deserialize, Serialize, StructDoc)]
//! struct AppConfig {
//!     #[serde(flatten)]
//!     msg: MsgCfg,
//!
//!     /// Configuration of the asynchronous tokio runtime.
//!     #[serde(default)]
//!     threadpool: TokioCfg,
//! }
//!
//! impl AppConfig {
//!     fn threadpool(&self) -> TokioCfg {
//!         self.threadpool.clone()
//!     }
//!
//!     fn msg(&self) -> &MsgCfg {
//!         &self.msg
//!     }
//! }
//!
//! fn main() {
//!     Spirit::<Empty, AppConfig>::new()
//!         // Makes sure we have a runtime configured from the config.
//!         // If we don't do this, the pipeline below would insert a default Tokio runtime to make
//!         // it work. If you want to customize the runtime (like here), make sure to insert it
//!         // before any pipelines requiring it (otherwise you get the default one from them).
//!         .with_singleton(Tokio::from_cfg(AppConfig::threadpool))
//!         // Will install and possibly cancel and replace the future if the config changes.
//!         .with(Pipeline::new("Msg").extract_cfg(AppConfig::msg))
//!         // Just an empty body here.
//!         .run(|spirit| {
//!             // Usually, one would terminate by CTRL+C, but we terminate from here to make sure
//!             // the example finishes.
//!             spirit.terminate();
//!             Ok(())
//!         })
//! }
//! ```
//!
//! An alternative approach can be seen at [`handlers::ToFutureUnconfigured`].
//!
//! [`Fragment`]: spirit::fragment::Fragment

pub mod either;
pub mod handlers;
pub mod installer;
#[cfg(feature = "net")]
pub mod net;
pub mod runtime;

pub use crate::installer::FutureInstaller;
#[cfg(feature = "net")]
pub use crate::net::{TcpListen, TcpListenWithLimits, UdpListen};
pub use crate::runtime::Tokio;
