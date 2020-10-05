use err_context::AnyError;
use serde::Deserialize;
use reqwest::Client;
use spirit::prelude::*;
use spirit::{Empty, Pipeline, Spirit};
use spirit_reqwest::ReqwestClient;
use spirit_reqwest::futures::{AtomicClient, IntoClient};
use tokio::runtime::Runtime;

#[derive(Debug, Default, Deserialize)]
struct Cfg {
    #[serde(default)]
    client: ReqwestClient,
}

impl Cfg {
    fn client(&self) -> &ReqwestClient {
        &self.client
    }
}

const DEFAULT_CFG: &str = r#"
[client]
timeout = "5s"
enable-gzip = false
"#;

async fn make_request(client: &Client) -> Result<(), AnyError> {
    let page = client
        .get("https://www.rust-lang.org")
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    println!("{}", page);
    Ok(())
}

fn main() {
    env_logger::init();
    // The ::empty client would panic if used before it is configured
    let client = AtomicClient::empty();
    Spirit::<Empty, Cfg>::new()
        .config_defaults(DEFAULT_CFG)
        .with(
            Pipeline::new("http client")
                .extract_cfg(Cfg::client)
                .transform(IntoClient)
                .install(client.clone()),
        )
        .run(move |_| {
            let mut runtime = Runtime::new()?;
            // But by now, spirit already stored the configured client in there. Also, if we were
            // running for a longer time, it would replace it with a new one every time we change
            // the configuration.
            let client = client.client();
            runtime.block_on(make_request(&client))
        });
}
