extern crate env_logger;
extern crate hyper;
#[macro_use]
extern crate serde_derive;
extern crate spirit;
extern crate spirit_hyper;
extern crate spirit_tokio;

use std::collections::HashSet;
use std::sync::Arc;

use hyper::server::Builder;
use hyper::service::service_fn_ok;
use hyper::{Body, Request, Response};
use spirit::prelude::*;
use spirit_hyper::{BuildServer, HttpServer};
use spirit_tokio::Runtime;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Hash)]
struct Signature {
    signature: Option<String>,
}

#[derive(Default, Deserialize)]
struct Ui {
    msg: String,
}

#[derive(Default, Deserialize)]
struct Config {
    /// On which ports (and interfaces) to listen.
    ///
    /// With some additional configuration about listening, the http server...
    ///
    /// Also, signature of the given listening port.
    listen: HashSet<HttpServer<Signature>>,
    /// The UI (there's only the message to send).
    ui: Ui,
}

impl Config {
    /// A function to extract the HTTP servers configuration
    fn listen(&self) -> &HashSet<HttpServer<Signature>> {
        &self.listen
    }
}

const DEFAULT_CONFIG: &str = r#"
[[listen]]
port = 1234
http-mode = "http1-only"
tcp-keepalive = "20s"
backlog = 30
only-v6 = true

[[listen]]
port = 5678
host = "127.0.0.1"
signature = "local"

[ui]
msg = "Hello world"
"#;

fn hello(
    spirit: &Arc<Spirit<Empty, Config>>,
    cfg: &Arc<HttpServer<Signature>>,
    _req: Request<Body>,
) -> Response<Body> {
    // Get some global configuration
    let mut msg = format!("{}\n", spirit.config().ui.msg);
    // Get some listener-local configuration.
    if let Some(ref signature) = cfg.transport.listener.extra_cfg.signature {
        msg.push_str(&format!("Brought to you by {}\n", signature));
    }
    Response::new(Body::from(msg))
}

fn main() {
    env_logger::init();
    Spirit::<Empty, _>::new()
        .config_defaults(DEFAULT_CONFIG)
        .config_exts(&["toml", "ini", "json"])
        .with_singleton(Runtime::default())
        .run(|spirit| {
            let spirit_srv = Arc::clone(spirit);
            let build_server =
                move |builder: Builder<_>, cfg: &HttpServer<Signature>, _: &'static str| {
                    let spirit = Arc::clone(&spirit_srv);
                    let cfg = Arc::new(cfg.clone());
                    builder.serve(move || {
                        let spirit = Arc::clone(&spirit);
                        let cfg = Arc::clone(&cfg);
                        service_fn_ok(move |req| hello(&spirit, &cfg, req))
                    })
                };
            spirit.with(
                Pipeline::new("listen")
                    .extract_cfg(Config::listen)
                    .transform(BuildServer(build_server)),
            )?;
            Ok(())
        });
}
