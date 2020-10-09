use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use err_context::AnyError;
use serde::{Deserialize, Serialize};
use spirit::{Empty, Pipeline, Spirit};
use spirit::prelude::*;
use spirit::fragment::driver::CacheEq;
use spirit_tokio::{FutureInstaller, Tokio};
use spirit_tokio::runtime::Cfg as TokioCfg;
use structdoc::StructDoc;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, StructDoc)]
#[serde(default)]
struct MsgCfg {
    /// A message to print now and then.
    msg: String,
    /// Time between printing the message.
    interval: Duration,
}

impl MsgCfg {
    async fn run(self) {
        loop {
            println!("{}", self.msg);
            tokio::time::delay_for(self.interval).await;
        }
    }
}

impl Default for MsgCfg {
    fn default() -> Self {
        MsgCfg {
            msg: "Hello".to_owned(),
            interval: Duration::from_secs(1),
        }
    }
}

spirit::simple_fragment! {
    impl Fragment for MsgCfg {
        type Driver = CacheEq<MsgCfg>;
        type Resource = Pin<Box<dyn Future<Output = ()> + Send>>;
        type Installer = FutureInstaller;
        fn create(&self, _: &'static str) -> Result<Self::Resource, AnyError> {
            let fut = self.clone().run();
            Ok(Box::pin(fut))
        }
    }
}

/// An application.
#[derive(Default, Deserialize, Serialize, StructDoc)]
struct AppConfig {
    #[serde(flatten)]
    msg: MsgCfg,

    /// Configuration of the asynchronous tokio runtime.
    #[serde(default)]
    threadpool: TokioCfg,
}

impl AppConfig {
    fn threadpool(&self) -> TokioCfg {
        self.threadpool.clone()
    }

    fn msg(&self) -> &MsgCfg {
        &self.msg
    }
}

fn main() {
    // You'd use spirit-log here instead probably, but we don't want to have cross-dependencies in
    // the example.
    env_logger::init();
    Spirit::<Empty, AppConfig>::new()
        .with_singleton(Tokio::from_cfg(AppConfig::threadpool))
        .with(Pipeline::new("Msg").extract_cfg(AppConfig::msg))
        .run(|_| {
            Ok(())
        })
}
