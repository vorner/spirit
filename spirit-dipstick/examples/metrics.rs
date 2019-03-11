use std::thread;
use std::time::Duration;

use dipstick::{stats_all, InputScope};
use log::debug;
use serde::Deserialize;
use spirit::prelude::*;
use spirit_dipstick::{Config as MetricsConfig, Monitor};

#[derive(Debug, Default, Deserialize)]
struct Cfg {
    metrics: MetricsConfig,
}

impl Cfg {
    fn metrics(&self) -> &MetricsConfig {
        &self.metrics
    }
}

const CFG: &str = r#"
[metrics]
prefix = "example" # If omitted, the name of the application is used
flush-period = "5s"  # Dump metric statistics every 5 seconds
backends = [
    { type = "stdout" },
]
"#;

fn main() {
    env_logger::init();
    let root = Monitor::new();

    Spirit::<Empty, Cfg>::new()
        .config_defaults(CFG)
        .with(
            Pipeline::new("metrics")
                .extract_cfg(Cfg::metrics)
                .install(root.installer(stats_all)),
        )
        .run(move |spirit| {
            let counter = root.counter("looped");
            while !spirit.is_terminated() {
                thread::sleep(Duration::from_millis(100));
                counter.count(1);
                debug!("tick");
            }
            Ok(())
        });
}
