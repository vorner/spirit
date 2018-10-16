#[macro_use]
extern crate log;
extern crate spirit;
extern crate spirit_log;
#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate structopt;

use std::thread;
use std::time::Duration;

use spirit::Spirit;
use spirit_log::{Cfg as LogCfg, Opts as LogOpts};

#[derive(Clone, Debug, StructOpt)]
struct Opts {
    #[structopt(flatten)]
    log: LogOpts,
}

impl Opts {
    fn log(&self) -> LogOpts {
        self.log.clone()
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
struct Ui {
    msg: String,
    sleep_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct Cfg {
    #[serde(flatten)]
    log: LogCfg,
    ui: Ui,
}

impl Cfg {
    fn log(&self) -> LogCfg {
        self.log.clone()
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
        .config_helper(Cfg::log, Opts::log, "logging")
        .run(|spirit| {
            while !spirit.is_terminated() {
                let cfg = spirit.config();
                info!("{}", cfg.ui.msg);
                thread::sleep(Duration::from_millis(cfg.ui.sleep_ms));
            }
            Ok(())
        });
}
