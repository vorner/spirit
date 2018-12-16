use serde_derive::Deserialize;
use spirit::{Empty, Spirit};
use spirit_reqwest::{AtomicClient, ReqwestClient};

#[derive(Debug, Default, Deserialize)]
struct Cfg {
    #[serde(default)]
    client: ReqwestClient,
}

impl Cfg {
    fn client(&self) -> ReqwestClient {
        self.client.clone()
    }
}

const DEFAULT_CFG: &str = r#"
[client]
timeout = "5s"
enable-gzip = false
"#;

fn main() {
    // The ::empty client would panic if used before it is configured
    let client = AtomicClient::empty();
    Spirit::<Empty, Cfg>::new()
        .config_defaults(DEFAULT_CFG)
        .config_helper(Cfg::client, &client, "client")
        .run(move |_| {
            // But by now, spirit already stored the configured client in there. Also, if we were
            // running for a longer time, it would replace it with a new one every time we change
            // the configuration.
            let page = client
                .get("https://www.rust-lang.org")
                .send()?
                .error_for_status()?
                .text()?;
            println!("{}", page);
            Ok(())
        });
}
