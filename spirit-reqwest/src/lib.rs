#![doc(
    html_root_url = "https://docs.rs/spirit-reqwest/0.2.2/spirit_reqwest/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! This helps with configuring the [`reqwest`] [`Client`].
//!
//! This is part of the [`spirit`] system.
//!
//! There are two levels of support. The first one is just letting the [`Spirit`] to load the
//! [`ReqwestClient`] configuration fragment and calling [`create`] or [`builder`] on it manually.
//!
//! The other, more convenient way, is pairing an extractor function with the [`AtomicClient`] and
//! letting [`Spirit`] keep an up to date version of [`Client`] in there at all times.
//!
//! # Examples
//!
//! ```rust
//! use serde::Deserialize;
//! use spirit::prelude::*;
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
//!         .with(Pipeline::new("http client").extract_cfg(Cfg::client).install(client.clone()))
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
//! [`create`]: ReqwestClient::create
//! [`builder`]: ReqwestClient::builder
//! [`Spirit`]: spirit::Spirit

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwapOption;
use failure::{Error, ResultExt};
use log::{debug, trace};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{
    Certificate, Client, ClientBuilder, Identity, IntoUrl, Method, Proxy, RedirectPolicy,
    RequestBuilder,
};
use serde::de::Deserializer;
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use serde_humantime::De;
use spirit::fragment::driver::CacheEq;
use spirit::fragment::Installer;
use spirit::utils::Hidden;
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

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

fn serialize_opt_dur<S: Serializer>(opt: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
    opt.as_ref()
        .map(|d| humantime::format_duration(*d).to_string())
        .serialize(s)
}

fn deserialize_opt_dur<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
    let dur = De::<Option<Duration>>::deserialize(d)?;
    Ok(dur.into_inner())
}

/// A configuration fragment to configure the reqwest [`Client`]
///
/// This carries configuration used to build a reqwest [`Client`]. An empty configuration
/// corresponds to default [`Client::new()`], but most things can be overridden.
///
/// The client can be created either manually by methods here, or by pairing it with
/// [`AtomicClient`]. See the [crate example](index.html#examples)
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
/// * `connect-timeout`: Timeout for the connection phase of a request (with units) or `nil` for no
///   such timeout. Default is no timeout.
/// * `max-idle-per-host`: Maximal number of idle connection per one host in the pool. Defaults to
///   `nil` (no limit).
/// * `http2-only`: Use only HTTP/2. Default is false (both HTTP/1 and HTTP/2 are allowed).
/// * `http1-case-sensitive-headers`: Consider HTTP/1 headers case sensitive.
/// * `local-address`: Make the requests from this address. Default is `nil`, which lets the OS to
///   choose.
/// * `http-proxy`: An URL of proxy that serves http requests.
/// * `https-proxy`: An URL of proxy that servers https requests.
/// * `redirects`: Number of allowed redirects per one request, `nil` to disable. Defaults to `10`.
/// * `referer`: Allow automatic setting of the referer header. Defaults to `true`.
/// * `tcp-nodelay`: Use the `SO_NODELAY` flag on all connections.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(structdoc::StructDoc))]
#[serde(rename_all = "kebab-case")]
pub struct ReqwestClient {
    /// Requires that all sockets used have the `SO_NODELAY` set.
    ///
    /// This improves latency in some cases at the cost of sending more packets.
    #[serde(default)]
    tcp_nodelay: bool,

    /// Additional certificates to add into the TLS trust store.
    ///
    /// Certificates in these files will be considered trusted in addition to the system trust
    /// store.
    ///
    /// Accepts PEM and DER formats (autodetected).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tls_extra_root_certs: Vec<PathBuf>,

    /// Client identity.
    ///
    /// A file with client certificate and private key that'll be used to authenticate against the
    /// server. This needs to be a PKCS12 format.
    ///
    /// If not set, no client identity is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    tls_identity: Option<PathBuf>,

    /// A password for the client identity file.
    ///
    /// If tls-identity is not set, the value here is ignored. If not set and the tls-identity is
    /// present, an empty password is attempted.
    #[serde(skip_serializing_if = "Option::is_none")]
    tls_identity_password: Option<Hidden<String>>,

    /// When validating the server certificate, accept even invalid or not matching hostnames.
    ///
    /// **DANGEROUS**
    ///
    /// Do not set unless you are 100% sure you have to and know what you're doing. This bypasses
    /// part of the protections TLS provides.
    ///
    /// Default is `false` (eg. invalid hostnames are not accepted).
    #[serde(default, skip_serializing_if = "is_false")]
    tls_accept_invalid_hostnames: bool,

    /// When validating the server certificate, accept even invalid or untrusted certificates.
    ///
    /// **DANGEROUS**
    ///
    /// Do not set unless you are 100% sure you have to and know what you're doing. This bypasses
    /// part of the protections TLS provides.
    ///
    /// Default is `false` (eg. invalid certificates are not accepted).
    #[serde(default, skip_serializing_if = "is_false")]
    tls_accept_invalid_certs: bool,

    /// Enables gzip transport compression.
    ///
    /// Default is on.
    #[serde(default = "default_gzip")]
    enable_gzip: bool,

    /// Headers added to each request.
    ///
    /// This can be used for example to add `User-Agent` header.
    ///
    /// By default no headers are added.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    default_headers: HashMap<String, String>,

    /// A whole-request timeout.
    ///
    /// If the request doesn't happen during this time, it gives up.
    ///
    /// The default is `30s`. Can be turned off by setting to `nil`.
    #[serde(
        deserialize_with = "deserialize_opt_dur",
        default = "default_timeout",
        serialize_with = "serialize_opt_dur"
    )]
    timeout: Option<Duration>,

    /// A timeout for connecting to the server.
    ///
    /// The default is no connection timeout.
    #[serde(
        deserialize_with = "deserialize_opt_dur",
        default,
        serialize_with = "serialize_opt_dur"
    )]
    connect_timeout: Option<Duration>,

    /// An URL for proxy to use on HTTP requests.
    ///
    /// No proxy is used if not set.
    #[structdoc(leaf = "URL")]
    #[serde(skip_serializing_if = "Option::is_none")]
    http_proxy: Option<SerdeUrl>,

    /// An URL for proxy to use on HTTPS requests.
    ///
    /// No proxy is used if not set.
    #[structdoc(leaf = "URL")]
    #[serde(skip_serializing_if = "Option::is_none")]
    https_proxy: Option<SerdeUrl>,

    /// How many redirects to allow for one request.
    ///
    /// The default value is 10. Support for redirects can be completely disabled by setting this
    /// to `nil`.
    #[serde(default = "default_redirects")]
    redirects: Option<usize>,

    /// Manages automatic setting of the Referer header.
    ///
    /// Default is on.
    #[serde(default = "default_referer")]
    referer: bool,

    /// Maximum number of idle connections per one host.
    ///
    /// Default is no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_idle_per_host: Option<usize>,

    /// Use only HTTP/2.
    ///
    /// Default is false.
    #[serde(default)]
    http2_only: bool,

    /// Use HTTP/1 headers in case sensitive manner.
    #[serde(default)]
    http1_case_sensitive_headers: bool,

    /// The local address connections are made from.
    ///
    /// Default is no address (the OS will choose).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    local_address: Option<IpAddr>,
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
            connect_timeout: None,
            http_proxy: None,
            https_proxy: None,
            redirects: default_redirects(),
            referer: default_referer(),
            http2_only: false,
            http1_case_sensitive_headers: false,
            max_idle_per_host: None,
            tcp_nodelay: false,
            local_address: None,
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
    /// [`create_client`]: ReqwestClient::create
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
            .connect_timeout(self.connect_timeout)
            .max_idle_per_host(self.max_idle_per_host.unwrap_or(usize::max_value()))
            .local_address(self.local_address)
            .default_headers(headers)
            .redirect(redirects)
            .referer(self.referer);
        if self.tcp_nodelay {
            builder = builder.tcp_nodelay();
        }
        if self.http2_only {
            builder = builder.h2_prior_knowledge();
        }
        if self.http1_case_sensitive_headers {
            builder = builder.http1_title_case_headers();
        }
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
    pub fn create_client(&self) -> Result<Client, Error> {
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
                self.0
                    .load()
                    .as_ref()
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
            .load_full()
            .expect("Accessing Reqwest HTTP client before setting it up")
    }

    /// Starts building an arbitrary request using the current client.
    ///
    /// This is forwarded to [`Client::request`].
    pub fn request<U: IntoUrl>(&self, method: Method, url: U) -> RequestBuilder {
        self.0
            .load()
            .as_ref()
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

spirit::simple_fragment! {
    impl Fragment for ReqwestClient {
        type Driver = CacheEq<ReqwestClient>;
        type Resource = Client;
        type Installer = ();
        fn create(&self, _: &'static str) -> Result<Client, Error> {
            self.create_client()
        }
    }
}

impl<O, C> Installer<Client, O, C> for AtomicClient {
    type UninstallHandle = ();
    fn install(&mut self, client: Client, name: &'static str) {
        debug!("Installing http client '{}'", name);
        self.replace(client);
    }
}
