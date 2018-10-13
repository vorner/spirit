extern crate failure;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate spirit;
extern crate spirit_daemonize;
#[macro_use]
extern crate structopt;

use std::thread;
use std::time::Duration;

use failure::Error;
use spirit::Spirit;
use spirit_daemonize::{Daemon, DaemonOpts};

#[derive(Clone, Debug, StructOpt)]
struct Opts {
    #[structopt(flatten)]
    daemon: DaemonOpts,
}

impl Opts {
    fn daemon(&self) -> &DaemonOpts {
        &self.daemon
    }
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

impl Cfg {
    fn daemon(&self) -> Daemon {
        self.daemon.clone()
    }
}

const DEFAULT_CONFIG: &str = r#"
[daemon]
pid_file = "/tmp/go_background.pid"
workdir = "/"

[ui]
msg = "Hello world"
sleep_ms = 100
"#;

fn main() -> Result<(), Error> {
    let (spirit, _, _) = Spirit::<Opts, Cfg>::new()
        .config_defaults(DEFAULT_CONFIG)
        .config_exts(&["toml", "ini", "json"])
        .config_helper(
            Cfg::daemon,
            spirit_daemonize::with_opts(Opts::daemon),
            "daemon",
        )
        .build(true)?;
    while !spirit.is_terminated() {
        let cfg = spirit.config();
        println!("{}", cfg.ui.msg);
        thread::sleep(Duration::from_millis(cfg.ui.sleep_ms));
    }
    Ok(())
}
