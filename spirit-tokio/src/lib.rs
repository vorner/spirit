#![doc(
    html_root_url = "https://docs.rs/spirit-tokio/0.6.1/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! Support for tokio inside spirit
//!
//! For now, this provides configuration for the runtime and an installer of futures.
//!
//! The previous versions also had [`Fragment`]s for configuring network primitives. These were for
//! tokio 0.1 and were not ported yet to support tokio 0.2. They may come back (hint: you can help
//! by PR).
//!
//! Note that this enables several features of [`tokio`].
//!
//! # Features
//!
//! * `rt-from-cfg`: Allows creating runtime from configuration. Enables the [`Tokio::FromCfg`]
//!   variant and [`Cfg`][crate::runtime::Cfg]. Enabled by default.
//! * `cfg-help`: Support for generating help for the configuration options.
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
//! use spirit_tokio::runtime::Cfg as TokioCfg;
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
//! [`Fragment`]: spirit::fragment::Fragment

pub mod installer;
pub mod runtime;

pub use crate::installer::FutureInstaller;
pub use crate::runtime::Tokio;
