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
    let client = AtomicClient::empty();
    Spirit::<Empty, Cfg>::new()
        .config_defaults(DEFAULT_CFG)
        .config_helper(Cfg::client, &client, "client")
        .run(move |_| {
            let page = client
                .get("https://www.rust-lang.org")
                .send()?
                .error_for_status()?
                .text()?;
            println!("{}", page);
            Ok(())
        });
}
