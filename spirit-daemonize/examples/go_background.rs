use std::thread;
use std::time::Duration;

use serde::Deserialize;
use spirit::prelude::*;
use spirit::Spirit;
use spirit_daemonize::{Daemon, Opts as DaemonOpts};
use structopt::StructOpt;

#[derive(Clone, Debug, StructOpt)]
struct Opts {
    #[structopt(flatten)]
    daemon: DaemonOpts,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct Ui {
    msg: String,
    sleep_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct Cfg {
    #[serde(default)]
    daemon: Daemon,
    ui: Ui,
}

const DEFAULT_CONFIG: &str = r#"
[daemon]
pid-file = "/tmp/go_background.pid"
workdir = "/"

[ui]
msg = "Hello world"
sleep_ms = 100
"#;

fn main() {
    Spirit::<Opts, Cfg>::new()
        .config_defaults(DEFAULT_CONFIG)
        .config_exts(&["toml", "ini", "json"])
        .with(Daemon::extension(|cfg: &Cfg, opts: &Opts| {
            opts.daemon.transform(cfg.daemon.clone())
        }))
        .run(|spirit| {
            while !spirit.is_terminated() {
                let cfg = spirit.config();
                println!("{}", cfg.ui.msg);
                thread::sleep(Duration::from_millis(cfg.ui.sleep_ms));
            }
            Ok(())
        });
}
