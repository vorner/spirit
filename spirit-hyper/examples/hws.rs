#![type_length_limit = "4194304"] // TODO What the hell is this?

extern crate failure;
extern crate hyper;
#[macro_use]
extern crate serde_derive;
extern crate spirit;
extern crate spirit_hyper;

use std::collections::HashSet;

use hyper::{Body, Request, Response};
use spirit::{Empty, Spirit, SpiritInner};
use spirit_hyper::HttpServer;

#[derive(Default, Deserialize)]
struct Ui {
    msg: String,
}

#[derive(Default, Deserialize)]
struct Config {
    /// On which ports (and interfaces) to listen.
    listen: HashSet<HttpServer>,
    /// The UI (there's only the message to send).
    ui: Ui,
}

impl Config {
    /// A function to extract the tcp ports configuration.
    fn listen(&self) -> HashSet<HttpServer> {
        self.listen.clone()
    }
}

const DEFAULT_CONFIG: &str = r#"
[[listen]]
port = 1234

[[listen]]
port = 5678
host = "localhost"

[ui]
msg = "Hello world"
"#;

fn hello(spirit: &SpiritInner<Empty, Config>, _: &Empty, _req: Request<Body>) -> Response<Body> {
    Response::new(Body::from(format!("{}\n", spirit.config().ui.msg)))
}

fn main() {
    Spirit::<_, Empty, _>::new(Config::default())
        .config_defaults(DEFAULT_CONFIG)
        .config_exts(&["toml", "ini", "json"])
        .config_helper(Config::listen, spirit_hyper::service_fn_ok(hello), "listen")
        .run(|_| Ok(()));
}
