#![doc(
    html_root_url = "https://docs.rs/spirit-reqwest/0.1.0/spirit_reqwest/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;
use std::fmt::{Debug, Display};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::{ArcSwapOption, Lease};
use failure::{Error, ResultExt};
use log::{debug, trace};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{
    Certificate, Client, Identity, IntoUrl, Method, Proxy, RedirectPolicy,
    RequestBuilder,
};
use serde::de::DeserializeOwned;
use serde_derive::Deserialize;
use spirit::Builder;
use spirit::helpers::CfgHelper;
use spirit::utils::Hidden;
use spirit::validation::Result as ValidationResult;
use structopt::StructOpt;
use url_serde::SerdeUrl;

fn default_timeout() -> Option<Duration> {
    Some(Duration::from_secs(30))
}

fn default_gzip() -> bool {
    true
}

fn default_redirects() -> Option<usize> {
    Some(10)
}

fn default_referer() -> bool {
    true
}

fn load_cert(path: &Path) -> Result<Certificate, Error> {
    let mut input = File::open(path)?;
    let mut cert = Vec::new();
    input.read_to_end(&mut cert)?;
    let result = if cert.starts_with(b"-----BEGIN CERTIFICATE-----") {
        trace!("Loading as PEM");
        Certificate::from_pem(&cert)?
    } else {
        trace!("Loading as DER");
        Certificate::from_der(&cert)?
    };
    Ok(result)
}

fn load_identity(path: &Path, passwd: &str) -> Result<Identity, Error> {
    let mut input = File::open(path)?;
    let mut identity = Vec::new();
    input.read_to_end(&mut identity)?;
    Ok(Identity::from_pkcs12_der(&identity, passwd)?)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct ReqwestClient {
    #[serde(default)]
    tls_extra_root_certs: Vec<PathBuf>,

    tls_identity: Option<PathBuf>,
    tls_identity_password: Option<Hidden<String>>,

    #[serde(default)]
    tls_accept_invalid_hostnames: bool,

    #[serde(default)]
    tls_accept_invalid_certs: bool,

    #[serde(default = "default_gzip")]
    enable_gzip: bool,

    #[serde(default)]
    default_headers: HashMap<String, String>,

    #[serde(with = "serde_humanize_rs", default = "default_timeout")]
    timeout: Option<Duration>,

    http_proxy: Option<SerdeUrl>,
    https_proxy: Option<SerdeUrl>,

    #[serde(default = "default_redirects")]
    redirects: Option<usize>,

    #[serde(default = "default_referer")]
    referer: bool,
}

impl Default for ReqwestClient {
    fn default() -> Self {
        ReqwestClient {
            tls_extra_root_certs: Vec::new(),
            tls_identity: None,
            tls_identity_password: None,
            tls_accept_invalid_hostnames: false,
            tls_accept_invalid_certs: false,
            enable_gzip: default_gzip(),
            default_headers: HashMap::new(),
            timeout: default_timeout(),
            http_proxy: None,
            https_proxy: None,
            redirects: default_redirects(),
            referer: default_referer(),
        }
    }
}

impl ReqwestClient {
    pub fn create(&self) -> Result<Client, Error> {
        debug!("Creating Reqwest client from {:?}", self);
        let mut headers = HeaderMap::new();
        for (key, val) in &self.default_headers {
            let name = HeaderName::from_bytes(key.as_bytes())
                .with_context(|_| format!("{} is not a valiad header name", key))?;
            let header = HeaderValue::from_bytes(val.as_bytes())
                .with_context(|_| format!("{} is not a valid header", val))?;
            headers.insert(name, header);
        }
        let redirects = match self.redirects {
            None => RedirectPolicy::none(),
            Some(limit) => RedirectPolicy::limited(limit),
        };
        let mut builder = Client::builder()
            .danger_accept_invalid_certs(self.tls_accept_invalid_certs)
            .danger_accept_invalid_hostnames(self.tls_accept_invalid_hostnames)
            .gzip(self.enable_gzip)
            .timeout(self.timeout)
            .default_headers(headers)
            .redirect(redirects)
            .referer(self.referer);
        for cert_path in &self.tls_extra_root_certs {
            trace!("Adding root certificate {:?}", cert_path);
            let cert = load_cert(cert_path)
                .with_context(|_| format!("Failed to load certificate {:?}", cert_path))?;
            builder = builder.add_root_certificate(cert);
        }
        if let Some(identity_path) = &self.tls_identity {
            trace!("Setting TLS client identity {:?}", identity_path);
            let passwd: &str = self
                .tls_identity_password
                .as_ref()
                .map(|s| s as &str)
                .unwrap_or_default();
            let identity = load_identity(&identity_path, passwd)
                .with_context(|_| format!("Failed to load identity {:?}", identity_path))?;
            builder = builder.identity(identity);
        }
        if let Some(proxy) = &self.http_proxy {
            let proxy_url = proxy.clone().into_inner();
            let proxy = Proxy::http(proxy_url)
                .with_context(|_| format!("Failed to configure http proxy to {:?}", proxy))?;
            builder = builder.proxy(proxy);
        }
        if let Some(proxy) = &self.http_proxy {
            let proxy_url = proxy.clone().into_inner();
            let proxy = Proxy::https(proxy_url)
                .with_context(|_| format!("Failed to configure https proxy to {:?}", proxy))?;
            builder = builder.proxy(proxy);
        }

        builder
            .build()
            .context("Failed to finish creating Reqwest HTTP client")
            .map_err(Error::from)
    }
}

#[derive(Clone, Debug)]
pub struct AtomicClient(Arc<ArcSwapOption<Client>>);

impl Default for AtomicClient {
    fn default() -> Self {
        Self::unconfigured()
    }
}

macro_rules! method {
    ($($(#[$attr: meta])* $name: ident();)*) => {
        $(
            $(#[$attr])*
            pub fn $name<U: IntoUrl>(&self, url: U) -> RequestBuilder {
                let lease = self.0.lease();
                Lease::get_ref(&lease)
                    .expect("Accessing Reqwest HTTP client before setting it up")
                    .$name(url)
            }
        )*
    }
}

impl AtomicClient {
    pub fn empty() -> Self {
        AtomicClient(Arc::new(ArcSwapOption::empty()))
    }
    pub fn unconfigured() -> Self {
        AtomicClient(Arc::new(ArcSwapOption::from_pointee(Client::new())))
    }
    pub fn replace<C: Into<Arc<Client>>>(&self, by: C) {
        let client = by.into();
        self.0.store(Some(client));
    }
    pub fn client(&self) -> Arc<Client> {
        self.0
            .load()
            .expect("Accessing Reqwest HTTP client before setting it up")
    }
    pub fn request<U: IntoUrl>(&self, method: Method, url: U) -> RequestBuilder {
        let lease = self.0.lease();
        Lease::get_ref(&lease)
            .expect("Accessing Reqwest HTTP client before setting it up")
            .request(method, url)
    }
    method! {
        get();
        post();
        put();
        patch();
        delete();
        head();
    }
}

impl<O, C, A: AsRef<AtomicClient>> CfgHelper<O, C, A> for ReqwestClient
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
{
    fn apply<Extractor, Name>(
        mut extractor: Extractor,
        atomic_client: A,
        name: Name,
        builder: Builder<O, C>,
    ) -> Builder<O, C>
    where
        Extractor: FnMut(&C) -> Self + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static
    {
        let cl = atomic_client.as_ref().clone();
        builder.config_validator(move |_, new_cfg, _| {
            let config = extractor(new_cfg);
            match config.create() {
                Ok(client) => {
                    let cl = cl.clone();
                    ValidationResult::nothing().on_success(move || cl.replace(Arc::new(client)))
                }
                Err(e) => {
                    let e = e.context(format!("Failed to create HTTP client {}", name));
                    ValidationResult::from_error(e.into())
                }
            }
        })
    }
}
