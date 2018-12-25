#![doc(
    html_root_url = "https://docs.rs/spirit-reqwest/0.1.0/spirit_reqwest/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! This helps with configuring the [`reqwest`][reqwest_crate] [`Client`].
//!
//! This is part of the [`spirit`][spirit_crate] system.
//!
//! There are two levels of support. The first one is just letting the [`Spirit`] to load the
//! [`ReqwestClient`] configuration fragment and calling [`create`] or [`builder`] on it manually.
//!
//! The other, more convenient way, is pairing an extractor function with the [`AtomicClient`] and
//! letting [`Spirit`] keep an up to date version of [`Client`] in there at all times. Together,
//! they form a [`CfgHelper`].
//!
//! # Examples
//!
//! ```rust
//! use serde_derive::Deserialize;
//! use spirit::{Empty, Spirit};
//! use spirit_reqwest::{AtomicClient, ReqwestClient};
//!
//! #[derive(Debug, Default, Deserialize)]
//! struct Cfg {
//!     #[serde(default)]
//!     client: ReqwestClient,
//! }
//!
//! impl Cfg {
//!     fn client(&self) -> ReqwestClient {
//!         self.client.clone()
//!     }
//! }
//!
//! fn main() {
//!     let client = AtomicClient::unconfigured(); // Get a default config before we get configured
//!     Spirit::<Empty, Cfg>::new()
//!         .config_helper(Cfg::client, &client, "client")
//!         .run(move |_| {
//!             let page = client
//!                 .get("https://www.rust-lang.org")
//!                 .send()?
//!                 .error_for_status()?
//!                 .text()?;
//!             println!("{}", page);
//!             Ok(())
//!         });
//! }
//! ```
//!
//! [reqwest_crate]: https://crates.io/crates/reqwest
//! [spirit_crate]: https://crates.io/crates/spirit
//! [`create`]: ReqwestClient::create
//! [`builder`]: ReqwestClient::builder
//! [`Spirit`]: spirit::Spirit

use std::borrow::Borrow;
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
use parking_lot::Mutex;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{
    Certificate, Client, ClientBuilder, Identity, IntoUrl, Method, Proxy, RedirectPolicy,
    RequestBuilder,
};
use serde::de::DeserializeOwned;
use serde_derive::Deserialize;
use spirit::helpers::CfgHelper;
use spirit::utils::Hidden;
use spirit::validation::Result as ValidationResult;
use spirit::Builder;
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
    const BEGIN_CERT: &[u8] = b"-----BEGIN CERTIFICATE-----";
    let contains_begin_cert = cert.windows(BEGIN_CERT.len()).any(|w| w == BEGIN_CERT);
    let result = if contains_begin_cert {
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

/// A configuration fragment to configure the reqwest [`Client`]
///
/// This carries configuration used to build a reqwest [`Client`]. An empty configuration
/// corresponds to default [`Client::new()`], but most things can be overridden.
///
/// The client can be created either manually by methods here, or by pairing it with
/// [`AtomicClient`] to form a [`CfgHelper`].
///
/// # Fields
///
/// * `extra-root-certs`: Array of paths, all will be loaded and *added* to the default
///   certification store. Can be either PEM or DER.
/// * `tls-identity`: A client identity to use to authenticate to the server. Needs to be a PKCS12
///   DER bundle. A password might be specified by the `tls-identity-password` field.
/// * `tls-accept-invalid-hostnames`: If set to true, it accepts invalid hostnames on https.
///   **Dangerous**, avoid if possible (default is `false`).
/// * `tls-accept-invalid-certs`: Allow accepting invalid https certificates. **Dangerous**, avoid
///   if possible (default is `false`).
/// * `enable-gzip`: Enable gzip compression of transferred data. Default is `true`.
/// * `default-headers`: A bundle of headers a request starts with. Map of name-value, defaults to
///   empty.
/// * `timeout`: Default whole-request timeout. Can be a time specification (with units) or `nil`
///   for no timeout. Default is `30s`.
/// * `http-proxy`: An URL of proxy that serves http requests.
/// * `https-proxy`: An URL of proxy that servers https requests.
/// * `redirects`: Number of allowed redirects per one request, `nil` to disable. Defaults to `10`.
/// * `referer`: Allow automatic setting of the referer header. Defaults to `true`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[cfg_attr(feature = "cfg-help", derive(structdoc::StructDoc))]
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

    #[structdoc(leaf)]
    http_proxy: Option<SerdeUrl>,
    #[structdoc(leaf)]
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
    /// Creates a pre-configured [`ClientBuilder`]
    ///
    /// This configures everything according to `self` and then returns the builder. The caller can
    /// modify it further and then create the client.
    ///
    /// Unless there's a need to tweak the configuration, the [`create`] is more comfortable.
    ///
    /// [`create`]: ReqwestClient::create
    pub fn builder(&self) -> Result<ClientBuilder, Error> {
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

        Ok(builder)
    }

    /// Creates a [`Client`] according to the configuration inside `self`.
    ///
    /// This is for manually creating the client. It is also possible to pair with an
    /// [`AtomicClient`] to form a [`CfgHelper`].
    pub fn create(&self) -> Result<Client, Error> {
        self.builder()?
            .build()
            .context("Failed to finish creating Reqwest HTTP client")
            .map_err(Error::from)
    }
}

/// A storage for one [`Client`] that can be atomically exchanged under the hood.
///
/// This acts as a proxy for a [`Client`]. This is cheap to clone all cloned handles refer to the
/// same client. It has most of the [`Client`]'s methods directly on itself, the others can be
/// accessed through the [`client`] method.
///
/// It also supports the [`replace`] method, by which it is possible to exchange the client inside.
///
/// While it can be used separately, it is best paired with a [`ReqwestClient`] configuration
/// fragment inside [`Spirit`] to have an up to date client around.
///
/// # Warning
///
/// As it is possible for the client to get replaced at any time by another thread, therefore
/// successive calls to eg. [`get`] may happen on different clients. If this is a problem, a caller
/// may get a specific client by the [`client`] method ‒ the client returned will not change for as
/// long as it is held (if the one inside here is replaced, both are kept alive until the return
/// value of [`client`] goes out of scope).
///
/// # Panics
///
/// Trying to access the client if the [`AtomicClient`] was created with [`empty`] and wasn't set
/// yet (either by [`Spirit`] or by explicit [`replace`]) will result into panic.
///
/// If you may use the client sooner, prefer either `default` or [`unconfigured`].
///
/// [`unconfigured`]: AtomicClient::unconfigured
/// [`Spirit`]: spirit::Spirit
/// [`replace`]: AtomicClient::replace
/// [`empty`]: AtomicClient::empty
/// [`client`]: AtomicClient::client
/// [`get`]: AtomicClient::get
#[derive(Clone, Debug)]
pub struct AtomicClient(Arc<ArcSwapOption<Client>>);

impl Default for AtomicClient {
    fn default() -> Self {
        Self::unconfigured()
    }
}

impl<C: Into<Arc<Client>>> From<C> for AtomicClient {
    fn from(c: C) -> Self {
        AtomicClient(Arc::new(ArcSwapOption::from(Some(c.into()))))
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
    /// Creates an empty [`AtomicClient`].
    ///
    /// This is effectively a `NULL`. It'll panic until a value is set, either by [`replace`]
    /// or by [`Spirit`] behind the scenes. It is appropriate if the caller is sure it will get
    /// configured before being accessed and creating an intermediate client first would be a
    /// waste.
    ///
    /// [`replace`]: AtomicClient::replace
    /// [`Spirit`]: spirit::Spirit
    pub fn empty() -> Self {
        AtomicClient(Arc::new(ArcSwapOption::empty()))
    }

    /// Creates an [`AtomicClient`] with default [`Client`] inside.
    pub fn unconfigured() -> Self {
        AtomicClient(Arc::new(ArcSwapOption::from_pointee(Client::new())))
    }

    /// Replaces the content of this [`AtomicClient`] with a new [`Client`].
    ///
    /// If you want to create a new [`AtomicClient`] out of a client, use [`From`]. This is meant
    /// for replacing the content of already existing ones.
    ///
    /// This replaces it for *all* connected handles (eg. created by cloning from the same
    /// original [`AtomicClient`]).
    pub fn replace<C: Into<Arc<Client>>>(&self, by: C) {
        let client = by.into();
        self.0.store(Some(client));
    }

    /// Returns a handle to the [`Client`] currently held inside.
    ///
    /// This serves a dual purpose:
    ///
    /// * If some functionality is not directly provided by the [`AtomicClient`] proxy.
    /// * If the caller needs to ensure a series of requests is performed using the same client.
    ///   While the content of the [`AtomicClient`] can change between calls to it, the content of
    ///   the [`Arc`] can't. While it is possible the client inside [`AtomicClient`] exchanged, the
    ///   [`Arc`] keeps its [`Client`] around (which may lead to multiple [`Client`]s in memory).
    pub fn client(&self) -> Arc<Client> {
        self.0
            .load()
            .expect("Accessing Reqwest HTTP client before setting it up")
    }

    /// Starts building an arbitrary request using the current client.
    ///
    /// This is forwarded to [`Client::request`].
    pub fn request<U: IntoUrl>(&self, method: Method, url: U) -> RequestBuilder {
        let lease = self.0.lease();
        Lease::get_ref(&lease)
            .expect("Accessing Reqwest HTTP client before setting it up")
            .request(method, url)
    }
    method! {
        /// Starts building a GET request.
        ///
        /// This is forwarded to [`Client::get`].
        get();

        /// Starts building a POST request.
        ///
        /// This is forwarded to [`Client::post`].
        post();

        /// Starts building a PUT request.
        ///
        /// This is forwarded to [`Client::put`].
        put();

        /// Starts building a PATCH request.
        ///
        /// This is forwarded to [`Client::patch`].
        patch();

        /// Starts building a DELETE request.
        ///
        /// This is forwarded to [`Client::delete`].
        delete();

        /// Starts building a HEAD request.
        ///
        /// This is forwarded to [`Client::head`].
        head();
    }
}

impl<O, C, A: Borrow<AtomicClient>> CfgHelper<O, C, A> for ReqwestClient
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
        Name: Clone + Display + Send + Sync + 'static,
    {
        let cl = atomic_client.borrow().clone();
        let last_config = Arc::new(Mutex::new(None));
        builder.config_validator(move |_, new_cfg, _| {
            let config = extractor(new_cfg);
            if last_config.lock().as_ref() == Some(&config) {
                debug!(
                    "Config of {} didn't change (still {:?}), not recreating the client",
                    name, config,
                );
                return ValidationResult::nothing();
            }
            match config.create() {
                Ok(client) => {
                    let cl = cl.clone();
                    let conf = Arc::clone(&last_config);
                    ValidationResult::nothing().on_success(move || {
                        *conf.lock() = Some(config);
                        cl.replace(Arc::new(client));
                    })
                }
                Err(e) => {
                    let e = e.context(format!("Failed to create HTTP client {}", name));
                    ValidationResult::from_error(e.into())
                }
            }
        })
    }
}
