use std::thread;
use std::time::Duration;

use log::info;
use serde::Deserialize;
use spirit::prelude::*;
use spirit_log::{Cfg as LogCfg, CfgAndOpts as LogBoth, Opts as LogOpts};
use structopt::StructOpt;

#[derive(Clone, Debug, StructOpt)]
struct Opts {
    #[structopt(flatten)]
    logging: LogOpts,
}

impl Opts {
    fn logging(&self) -> LogOpts {
        self.logging.clone()
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
struct Ui {
    msg: String,
    sleep_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct Cfg {
    #[serde(default, skip_serializing_if = "LogCfg::is_empty")]
    logging: LogCfg,
    ui: Ui,
}

impl Cfg {
    fn logging(&self) -> LogCfg {
        self.logging.clone()
    }
}

const DEFAULT_CONFIG: &str = r#"
[[logging]]
level = "INFO"
type = "stderr"

[[logging]]
level = "DEBUG"
type = "file"
filename = "/tmp/example.log"
clock = "UTC"

[ui]
msg = "Hello!"
sleep_ms = 100
"#;

fn main() {
    Spirit::<Opts, Cfg>::new()
        .config_defaults(DEFAULT_CONFIG)
        .config_exts(&["toml", "ini", "json"])
        .with(
            Pipeline::new("logging").extract(|opts: &Opts, cfg: &Cfg| LogBoth {
                cfg: cfg.logging(),
                opts: opts.logging(),
            }),
        )
        .run(|spirit| {
            while !spirit.is_terminated() {
                let cfg = spirit.config();
                info!("{}", cfg.ui.msg);
                thread::sleep(Duration::from_millis(cfg.ui.sleep_ms));
            }
            Ok(())
        });
}
