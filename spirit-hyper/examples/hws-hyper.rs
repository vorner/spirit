use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::Arc;

use hyper::server::Builder;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response};
use serde::Deserialize;
use spirit::prelude::*;
use spirit::{Empty, Pipeline, Spirit};
use spirit_hyper::{BuildServer, HttpServer};
use spirit_tokio::runtime::Tokio;
use tokio::sync::oneshot::Receiver;

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

async fn hello(
    spirit: &Arc<Spirit<Empty, Config>>,
    cfg: &Arc<HttpServer<Signature>>,
    _req: Request<Body>,
) -> Result<Response<Body>, Infallible> {
    // Get some global configuration
    let mut msg = format!("{}\n", spirit.config().ui.msg);
    // Get some listener-local configuration.
    if let Some(ref signature) = cfg.transport.listen.extra_cfg.signature {
        msg.push_str(&format!("Brought to you by {}\n", signature));
    }
    Ok(Response::new(Body::from(msg)))
}

fn main() {
    // You could use spirit_log instead to gain better configurability
    env_logger::init();

    Spirit::<Empty, _>::new()
        .config_defaults(DEFAULT_CONFIG)
        .config_exts(&["toml", "ini", "json"])
        // Explicitly enabling tokio integration, implicitly it would happen inside run and that's
        // too late.
        .with_singleton(Tokio::default()) // Explicitly enabling tokio
        .run(|spirit| {
            let spirit_srv = Arc::clone(spirit);
            let build_server = move |builder: Builder<_>,
                                     cfg: &HttpServer<Signature>,
                                     _: &'static str,
                                     shutdown: Receiver<()>| {
                let spirit = Arc::clone(&spirit_srv);
                let cfg = Arc::new(cfg.clone());
                builder
                    .serve(make_service_fn(move |_conn| {
                        let spirit = Arc::clone(&spirit);
                        let cfg = Arc::clone(&cfg);
                        async move {
                            let spirit = Arc::clone(&spirit);
                            let cfg = Arc::clone(&cfg);
                            Ok::<_, Infallible>(service_fn(move |req| {
                                let spirit = Arc::clone(&spirit);
                                let cfg = Arc::clone(&cfg);
                                async move { hello(&spirit, &cfg, req).await }
                            }))
                        }
                    }))
                    .with_graceful_shutdown(async move {
                        let _ = shutdown.await; // Throw away errors
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
