#![doc(
    html_root_url = "https://docs.rs/spirit-tokio/0.2.0/spirit_tokio/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! A collection of helpers integrating tokio primitives into [spirit]
//!
//! The crate provides few helper implementations that handle listening socket auto-reconfiguration
//! based on configuration.
//!
//! # Examples
//!
//! ```rust
//! extern crate failure;
//! extern crate serde;
//! #[macro_use]
//! extern crate serde_derive;
//! extern crate spirit;
//! extern crate spirit_tokio;
//! extern crate tokio;
//!
//! use std::sync::Arc;
//!
//! use failure::Error;
//! use spirit::{Empty, Spirit};
//! use spirit_tokio::TcpListen;
//! use tokio::net::TcpStream;
//! use tokio::prelude::*;
//!
//! const DEFAULT_CONFIG: &str = r#"
//! [listening_socket]
//! port = 1234
//! "#;
//! #[derive(Default, Deserialize)]
//! struct Config {
//!     listening_socket: TcpListen,
//! }
//!
//! impl Config {
//!     fn listening_socket(&self) -> TcpListen {
//!         self.listening_socket.clone()
//!     }
//! }
//!
//! fn connection(_: &Arc<Spirit<Empty, Config>>, conn: TcpStream, _: &Empty)
//!     -> impl Future<Item = (), Error = Error>
//! {
//!     tokio::io::write_all(conn, "Hello\n")
//!         .map(|_| ())
//!         .map_err(Error::from)
//! }
//!
//! fn main() {
//!     Spirit::<Empty, Config>::new()
//!         .config_defaults(DEFAULT_CONFIG)
//!         .config_helper(Config::listening_socket, connection, "Listener")
//!         .run(|spirit| {
//! #           let spirit = Arc::clone(spirit);
//! #           std::thread::spawn(move || spirit.terminate());
//!             Ok(())
//!         });
//! }
//! ```
//!
//! Further examples are in the
//! [git repository](https://github.com/vorner/spirit/tree/master/spirit-tokio/examples).
//!
//! [spirit]: https://crates.io/crates/spirit.

extern crate failure;
extern crate futures;
#[macro_use]
extern crate log;
extern crate parking_lot;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate spirit;
extern crate structopt;
extern crate tk_listen;
extern crate tokio;

use std::fmt::{Debug, Display};
use std::io::Error as IoError;
use std::iter;
use std::net::{TcpListener as StdTcpListener, UdpSocket as StdUdpSocket};
use std::sync::Arc;
use std::time::Duration;

use failure::Error;
use futures::sync::{mpsc, oneshot};
use futures::{Future, IntoFuture};
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use spirit::helpers::{CfgHelper, Helper, IteratedCfgHelper};
use spirit::validation::{Result as ValidationResult, Results as ValidationResults};
use spirit::{Builder, Empty, Spirit};
use structopt::StructOpt;
use tk_listen::ListenExt;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::prelude::*;
use tokio::reactor::Handle;
use tokio::runtime;

// TODO: Make this public, it may be useful to other helper crates.
struct RemoteDrop {
    request_drop: Option<oneshot::Sender<()>>,
    drop_confirmed: Option<oneshot::Receiver<()>>,
}

impl Drop for RemoteDrop {
    fn drop(&mut self) {
        trace!("Requesting remote drop");
        // Ask the other side to drop the thing
        let _ = self.request_drop.take().unwrap().send(());
        // And wait for it to actually happen
        let _ = self.drop_confirmed.take().unwrap().wait();
        trace!("Remote drop done");
    }
}

pub trait Name: AsRef<str> + Clone + Send + Sync + 'static {}
impl<N> Name for N where N: AsRef<str> + Clone + Send + Sync + 'static {}

pub trait ExtraCfgCarrier {
    type Extra;
    fn extra(&self) -> &Self::Extra;
}

// TODO: Are all these trait bounds necessary?
pub trait ResourceConfig<O, C>:
    Debug + ExtraCfgCarrier + Sync + Send + PartialEq + 'static
{
    type Seed: Send + Sync + 'static;
    type Resource: Send + 'static;
    fn create(&self, name: &str) -> Result<Self::Seed, Error>;
    fn fork(&self, seed: &Self::Seed, name: &str) -> Result<Self::Resource, Error>;
    fn scaled(&self, name: &str) -> (usize, ValidationResults) {
        (1, ValidationResults::new())
    }
    fn is_similar(&self, other: &Self, name: &str) -> bool {
        self == other
    }
    fn install<N: Name>(builder: Builder<O, C>, _name: &N) -> Builder<O, C> {
        builder
    }
}

pub trait ResourceConsumer<Config, O, C>: Sync + Send + 'static
where
    Config: ResourceConfig<O, C>,
{
    type Error: Display;
    type Future: Future<Item = (), Error = Self::Error> + Send + 'static;
    fn build_future(
        &self,
        spirit: &Arc<Spirit<O, C>>,
        config: &Arc<Config>,
        resource: Config::Resource,
        name: &str,
    ) -> Self::Future;
    fn install<N: Name>(builder: Builder<O, C>, _name: &N) -> Builder<O, C> {
        builder
    }
}

impl<F, Config, O, C, R> ResourceConsumer<Config, O, C> for F
where
    F: Fn(&Arc<Spirit<O, C>>, &Arc<Config>, Config::Resource, &str) -> R + Sync + Send + 'static,
    Config: ResourceConfig<O, C>,
    R: IntoFuture<Item = ()>,
    R::Error: Display,
    R::Future: Sync + Send + 'static,
{
    type Error = R::Error;
    type Future = R::Future;
    fn build_future(
        &self,
        spirit: &Arc<Spirit<O, C>>,
        config: &Arc<Config>,
        resource: Config::Resource,
        name: &str,
    ) -> R::Future {
        self(spirit, config, resource, name).into_future()
    }
}

pub trait IntoIncoming: Send + Sync + 'static {
    type Connection: Send + Sync + 'static;
    type Incoming: Stream<Item = Self::Connection, Error = IoError> + Send + Sync + 'static;
    fn into_incoming(self) -> Self::Incoming;
}

impl IntoIncoming for TcpListener {
    type Connection = TcpStream;
    type Incoming = tokio::net::tcp::Incoming;
    fn into_incoming(self) -> Self::Incoming {
        self.incoming()
    }
}

pub trait ListenInfo<O, C>: ResourceConfig<O, C> {
    fn error_sleep(&self) -> Duration;
    fn max_conn(&self) -> usize;
}

pub fn per_connection<Config, F, R, O, C>(action: F) -> impl ResourceConsumer<Config, O, C>
where
    Config: ListenInfo<O, C>,
    Config::Resource: IntoIncoming,
    F: Fn(&Arc<Spirit<O, C>>, &Arc<Config>, <Config::Resource as IntoIncoming>::Connection) -> R
        + Sync
        + Send
        + 'static,
    R: IntoFuture<Item = ()>,
    R::Error: Display,
    R::Future: Sync + Send + 'static,
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
{
    let action = Arc::new(action);
    move |spirit: &_, config: &Arc<Config>, listener: Config::Resource, name: &str| {
        let spirit = Arc::clone(spirit);
        let config = Arc::clone(config);
        let max_conn = config.max_conn();
        let action = Arc::clone(&action);
        let name: Arc<str> = Arc::from(name.to_owned());
        listener
            .into_incoming()
            .sleep_on_error(config.error_sleep())
            .map(move |new_conn| {
                // The listen below keeps track of how many parallel connections
                // there are. But it does so inside the same future, which prevents
                // the separate connections to be handled in parallel on a thread
                // pool. So we spawn the future to handle the connection itself.
                // But we want to keep the future alive so the listen doesn't think
                // it already terminated, therefore the done-channel.
                let (done_send, done_recv) = oneshot::channel();
                let name_err = Arc::clone(&name);
                let handle_conn = action(&spirit, &config, new_conn)
                    .into_future()
                    .then(move |r| {
                        if let Err(e) = r {
                            error!("Failed to handle connection on {:?}: {}", name_err, e);
                        }
                        // Ignore the other side going away. This may happen if the
                        // listener terminated, but the connection lingers for
                        // longer.
                        let _ = done_send.send(());
                        future::ok(())
                    });
                tokio::spawn(handle_conn);
                done_recv.then(|_| future::ok(()))
            })
            .listen(max_conn)
            .map_err(|()| -> Error {
                unreachable!("tk-listen never errors");
            })
    }
}

// TODO: Cut it into smaller pieces
pub fn resources<Config, Consumer, E, R, O, C, N>(
    mut extract: E,
    consumer: Consumer,
    name: N,
) -> impl Helper<O, C>
where
    Config: ResourceConfig<O, C>,
    Consumer: ResourceConsumer<Config, O, C>,
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    E: FnMut(&C) -> R + Sync + Send + 'static,
    R: IntoIterator<Item = Config>,
    N: Name,
{
    struct Install<O, C, Config: ResourceConfig<O, C>> {
        resource: Config::Resource,
        drop_req: oneshot::Receiver<()>,
        confirm_drop: oneshot::Sender<()>,
        config: Arc<Config>,
    }

    let (install_sender, install_receiver) = mpsc::unbounded::<Install<_, _, Config>>();
    let name_validator = name.clone();
    let name_builder = name.clone();
    let installer = move |spirit: &Arc<Spirit<O, C>>| {
        let spirit = Arc::clone(spirit);
        install_receiver.for_each(move |install| {
            let Install {
                resource,
                drop_req,
                confirm_drop,
                config,
            } = install;
            let cfg_str: Arc<str> = Arc::from(format!("{:?}", config));
            let cfg_str_err = Arc::clone(&cfg_str);
            let name = name.clone();
            let name_err = name.clone();
            debug!(
                "Installing resource {} with config {}",
                name.as_ref(),
                cfg_str
            );

            let task = consumer
                .build_future(&spirit, &config, resource, name.as_ref())
                .map_err(move |e| {
                    error!(
                        "Task {} on config {} failed: {}",
                        name_err.as_ref(),
                        cfg_str_err,
                        e
                    );
                })
                .select(drop_req.map_err(|_| ())) // Cancelation is OK too
                .then(move |orig| {
                    debug!("Terminated resource {} on cfg {}", name.as_ref(), cfg_str);
                    drop(orig); // Make sure the original future is dropped first.
                    confirm_drop.send(())
                })
                .map_err(|_| ()); // If nobody waits for confirm_drop, that's OK.

            tokio::spawn(task)
        })
    };

    struct CacheEntry<O, C, Config: ResourceConfig<O, C>> {
        config: Arc<Config>,
        seed: Arc<Config::Seed>,
        remote: Vec<Arc<RemoteDrop>>,
    }

    // It doesn't want to auto-implement the trait because of the type parameter, but we want clone
    // on Arcs only!
    impl<O, C, Config: ResourceConfig<O, C>> Clone for CacheEntry<O, C, Config> {
        fn clone(&self) -> Self {
            CacheEntry {
                config: Arc::clone(&self.config),
                seed: Arc::clone(&self.seed),
                remote: self.remote.clone(),
            }
        }
    }

    let cache = Arc::new(Mutex::new(Vec::<CacheEntry<O, C, Config>>::new()));
    let validator = move |_: &Arc<C>, cfg: &mut C, _: &O| -> ValidationResults {
        let mut results = ValidationResults::new();
        let orig_cache = cache.lock();
        let mut new_cache = Vec::new();
        let mut to_send = Vec::new();

        for cfg in extract(cfg) {
            let cfg = Arc::new(cfg);
            let previous = orig_cache
                .iter()
                .find(|orig| cfg.is_similar(&orig.config, name_validator.as_ref()))
                .map(CacheEntry::clone); // Just a bunch of Arcs to clone

            let mut cache = if let Some(prev) = previous {
                prev
            } else {
                let seed = match cfg.create(name_validator.as_ref()) {
                    Ok(seed) => Arc::new(seed),
                    Err(e) => {
                        results.merge(ValidationResult::from(e));
                        continue;
                    }
                };
                CacheEntry {
                    config: Arc::clone(&cfg),
                    seed,
                    remote: Vec::new(),
                }
            };

            if cache.config != cfg {
                cache.remote.clear();
            }

            let (scale, scale_validation) = cfg.scaled(name_validator.as_ref());
            results.merge(scale_validation);
            assert!(scale >= cache.remote.len());
            for _ in 0..scale - cache.remote.len() {
                let resource = match cfg.fork(&cache.seed, name_validator.as_ref()) {
                    Ok(resource) => resource,
                    Err(e) => {
                        results.merge(ValidationResult::from(e));
                        continue;
                    }
                };

                let (drop_send, drop_recv) = oneshot::channel();
                let (confirm_send, confirm_recv) = oneshot::channel();
                cache.remote.push(Arc::new(RemoteDrop {
                    request_drop: Some(drop_send),
                    drop_confirmed: Some(confirm_recv),
                }));
                to_send.push(Install {
                    config: Arc::clone(&cfg),
                    confirm_drop: confirm_send,
                    drop_req: drop_recv,
                    resource,
                });
            }
            cache.config = cfg;
            new_cache.push(cache);
        }
        let cache = Arc::clone(&cache);
        let name = name_validator.clone();
        let sender = install_sender.clone();
        results.merge(ValidationResult::nothing().on_success(move || {
            for install in to_send {
                trace!(
                    "Sending {}/{:?} to the reactor",
                    name.as_ref(),
                    install.config
                );
                sender
                    .unbounded_send(install)
                    .expect("The tokio end got dropped");
            }
            *cache.lock() = new_cache;
            debug!("New version of {} sent", name.as_ref());
        }));
        results
    };

    move |builder: Builder<O, C>| {
        let builder = Config::install(builder, &name_builder);
        let builder = Consumer::install(builder, &name_builder);
        builder
            .config_validator(validator)
            .with_singleton(Runtime::default())
            .before_body(move |spirit| {
                tokio::spawn(installer(spirit));
                Ok(())
            })
    }
}

pub fn resource<Config, Consumer, E, O, C, N>(
    mut extract: E,
    consumer: Consumer,
    name: N,
) -> impl Helper<O, C>
where
    Config: ResourceConfig<O, C>,
    Consumer: ResourceConsumer<Config, O, C>,
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    E: FnMut(&C) -> Config + Sync + Send + 'static,
    N: Name,
{
    resources(move |c: &C| iter::once(extract(c)), consumer, name)
}

/// An inner tokio task helper.
///
/// This is mostly used internally, but it is also made public as it may be useful for authors of
/// other tokio-related helpers.
///
/// There are several stages with some tokio resource. First, the configuration is extracted using
/// the `extract` closure. Note that the closure should return an iterator of configurations. Then,
/// it is turned into a base resource by the `build` closure. The resource is then sent into the
/// tokio runtime where the `to_task` is run on it, to turn it into a future/task to be spawned on
/// the runtime.
///
/// See the bounds on the `Helper` trait implementation for exact signatures.
pub struct Task<Extract, Build, ToTask, Name> {
    /// A closure used to extract configuration.
    pub extract: Extract,
    /// A closure to turn one bit of configuration into some kind of resource.
    pub build: Build,
    /// Wraps a resource, adds some activity around it and returns a future to be spawned onto
    /// tokio.
    pub to_task: ToTask,
    /// A name used in logging.
    pub name: Name,
}

impl<O, C, SubCfg, Resource, Extract, ExtractIt, ExtraCfg, Build, ToTask, InnerTask, Name>
    Helper<O, C> for Task<Extract, Build, ToTask, Name>
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    Extract: FnMut(&C) -> ExtractIt + Send + 'static,
    ExtractIt: IntoIterator<Item = (SubCfg, ExtraCfg, usize, ValidationResults)>,
    ExtraCfg: Clone + Debug + PartialEq + Send + 'static,
    SubCfg: Clone + Debug + PartialEq + Send + 'static,
    Build: FnMut(&SubCfg) -> Result<Resource, Error> + Send + 'static,
    Resource: Clone + Send + 'static,
    ToTask: FnMut(&Arc<Spirit<O, C>>, Resource, ExtraCfg) -> InnerTask + Send + 'static,
    InnerTask: IntoFuture<Item = (), Error = Error> + Send + 'static,
    <InnerTask as IntoFuture>::Future: Send,
    Name: Clone + Display + Send + Sync + 'static,
{
    fn apply(self, builder: Builder<O, C>) -> Builder<O, C> {
        let Task {
            mut extract,
            mut build,
            mut to_task,
            name,
        } = self;
        debug!("Installing helper {}", name);
        // Note: this depends on the specific drop order to avoid races
        struct Install<R, ExtraCfg> {
            resource: R,
            drop_req: oneshot::Receiver<()>,
            confirm_drop: oneshot::Sender<()>,
            cfg: String,
            extra_conf: ExtraCfg,
        }
        #[derive(Clone)]
        struct Cache<SubCfg, ExtraCfg, Resource> {
            sub_cfg: SubCfg,
            extra_cfg: ExtraCfg,
            resource: Resource,
            remote: Vec<Arc<RemoteDrop>>,
        }
        let (install_sender, install_receiver) = mpsc::unbounded::<Install<Resource, ExtraCfg>>();

        let installer_name = name.clone();
        let installer = move |spirit: &Arc<Spirit<O, C>>| {
            let spirit = Arc::clone(spirit);
            install_receiver.for_each(move |install| {
                let Install {
                    resource,
                    drop_req,
                    confirm_drop,
                    cfg,
                    extra_conf,
                } = install;
                let name = installer_name.clone();
                debug!("Installing resource {} with config {}", name, cfg);
                // Get the task itself
                let task = to_task(&spirit, resource, extra_conf).into_future();
                let err_name = name.clone();
                let err_cfg = cfg.clone();
                // Wrap it in the cancelation routine
                let wrapped = task
                    .map_err(move |e| error!("Task {} on cfg {} failed: {}", err_name, err_cfg, e))
                    .select(drop_req.map_err(|_| ())) // Cancelation is OK too
                    .then(move |orig| {
                        debug!("Terminated resource {} on cfg {}", name, cfg);
                        drop(orig); // Make sure the original future is dropped first.
                        confirm_drop.send(())
                    })
                    .map_err(|_| ()); // If nobody waits for confirm_drop, that's OK.
                tokio::spawn(wrapped)
            })
        };

        let cache = Arc::new(Mutex::new(Vec::new()));
        let validator = move |_: &Arc<C>, cfg: &mut C, _: &O| -> ValidationResults {
            let mut results = ValidationResults::new();
            let orig_cache = cache.lock();
            let mut new_cache = Vec::new();
            let mut to_send = Vec::new();
            for sub in extract(cfg) {
                let (sub, extra, mut scale, sub_results) = sub;
                results.merge(sub_results);

                let previous = orig_cache
                    .iter()
                    .find(|Cache { sub_cfg, .. }| sub_cfg == &sub);

                let mut cached = if let Some(previous) = previous {
                    debug!("Reusing previous instance of {} for {:?}", name, sub);
                    previous.clone()
                } else {
                    trace!("Creating new instance of {} for {:?}", name, sub);
                    match build(&sub) {
                        Ok(resource) => {
                            debug!("Successfully created instance of {} for {:?}", name, sub);
                            Cache {
                                sub_cfg: sub.clone(),
                                extra_cfg: extra.clone(),
                                resource,
                                remote: Vec::new(),
                            }
                        }
                        Err(e) => {
                            let msg = format!("Creationg of {} for {:?} failed", name, sub);
                            // The error will appear together for the validator
                            debug!("{}: {}", msg, e);
                            results.merge(ValidationResult::from_error(e.context(msg).into()));
                            continue;
                        }
                    }
                };

                if extra != cached.extra_cfg {
                    debug!(
                        "Extra config for {:?} differs (old: {:?}, new: {:?}",
                        sub, cached.extra_cfg, extra
                    );
                    // If we have no old remotes here, they'll get dropped on installation and
                    // we'll „scale up“ to the current scale.
                    cached.remote.clear();
                }

                if cached.remote.len() > scale {
                    debug!(
                        "Scaling down {} from {} to {}",
                        name,
                        cached.remote.len(),
                        scale
                    );
                    cached.remote.drain(scale..);
                }

                if cached.remote.len() < scale {
                    debug!(
                        "Scaling up {} from {} to {}",
                        name,
                        cached.remote.len(),
                        scale
                    );
                    while cached.remote.len() < scale {
                        let (req_sender, req_recv) = oneshot::channel();
                        let (confirm_sender, confirm_recv) = oneshot::channel();
                        to_send.push(Install {
                            resource: cached.resource.clone(),
                            drop_req: req_recv,
                            confirm_drop: confirm_sender,
                            cfg: format!("{:?}", sub),
                            extra_conf: extra.clone(),
                        });
                        cached.remote.push(Arc::new(RemoteDrop {
                            request_drop: Some(req_sender),
                            drop_confirmed: Some(confirm_recv),
                        }));
                    }
                }

                new_cache.push(cached);
            }

            let sender = install_sender.clone();
            let cache = Arc::clone(&cache);
            let name = name.clone();
            results.merge(ValidationResult::nothing().on_success(move || {
                for install in to_send {
                    trace!("Sending {} to the reactor", install.cfg);
                    sender
                        .unbounded_send(install)
                        .expect("The tokio end got dropped");
                }
                *cache.lock() = new_cache;
                debug!("New version of {} sent", name);
            }));
            results
        };

        builder
            .config_validator(validator)
            .with_singleton(Runtime::default())
            .before_body(move |spirit| {
                tokio::spawn(installer(spirit));
                Ok(())
            })
    }
}

fn default_host() -> String {
    "::".to_owned()
}

fn default_scale() -> usize {
    1
}

/// A description of listening interface and port.
///
/// This can be used as part of configuration to describe a socket.
///
/// Note that the `Default` implementation is there to provide something on creation of `Spirit`,
/// but isn't very useful.
///
/// It contains these configuration options:
///
/// * `port` (mandatory)
/// * `host` (optional, if not present, `*` is used)
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Listen {
    port: u16,
    #[serde(default = "default_host")]
    host: String,
}

impl Default for Listen {
    fn default() -> Self {
        Listen {
            port: 0,
            host: default_host(),
        }
    }
}

impl Listen {
    /// Creates a TCP socket described by the loaded configuration.
    pub fn create_tcp(&self) -> Result<StdTcpListener, Error> {
        Ok(StdTcpListener::bind((&self.host as &str, self.port))?)
    }
    /// Creates a UDP socket described by the loaded configuration.
    pub fn create_udp(&self) -> Result<StdUdpSocket, Error> {
        Ok(StdUdpSocket::bind((&self.host as &str, self.port))?)
    }
}

fn default_error_sleep() -> u64 {
    100
}

fn default_max_conn() -> usize {
    1000
}

/// Description of scaling into multiple tasks.
///
/// The helpers in this crate allow creating multiple copies of the socket. If using the default
/// (threadpool) tokio executor, it makes it possible to share the load across multiple threads.
///
/// Note that in case of TCP, even if there's just one instance of the listening sockets, the
/// individual connections still can be handled by multiple threads, as each accepted connection is
/// a separate task. However, if you use UDP or have too many (lightweight) connections to saturate
/// the single listening instance, having more than one can help.
///
/// While it is possible to define on `Scaled` types, it is expected the provided ones should be
/// enough.
pub trait Scaled {
    /// Returns how many instances there should be.
    ///
    /// And accompanies it with optional validation results, to either refuse or warn about the
    /// configuration.
    fn scaled<Name: Display>(&self, name: Name) -> (usize, ValidationResults);
}

/// A scaling configuration provided by user.
///
/// This contains a single option `scale` (which is embedded inside the relevant configuration
/// section), specifying the number of parallel instances. If the configuration is not provided,
/// this contains `1`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Scale {
    #[serde(default = "default_scale")]
    scale: usize,
}

impl Default for Scale {
    fn default() -> Self {
        Scale { scale: 1 }
    }
}

impl Scaled for Scale {
    fn scaled<Name: Display>(&self, name: Name) -> (usize, ValidationResults) {
        if self.scale > 0 {
            (self.scale, ValidationResults::new())
        } else {
            let msg = format!("Turning scale in {} from 0 to 1", name);
            (1, ValidationResult::warning(msg).into())
        }
    }
}

/// Turns scaling off and provides a single instance.
///
/// This contains no configuration options and work just as a „plug“ type to fill in a type
/// parameter.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Singleton {}

impl Scaled for Singleton {
    fn scaled<Name: Display>(&self, _: Name) -> (usize, ValidationResults) {
        (1, ValidationResults::new())
    }
}

/// An alternative (more concrete) trait to `IteratedCfgHelper` for futures/tokio integration.
///
/// While the `IteratedCfgHelper` is good for end-user integration with extractors, there are too
/// many loose ends to be usable to reuse in implementing composed helpers.
///
/// This has more concrete parts (specifically, it mandates that it produces only one specific type
/// of something), so higher level abstractions can reuse lower-level builders. The current use
/// case is being able to wrap the creator of TcpStream (or other streams, like possible SSL
/// stream, Unix domain streams, etc) to be able to put something on top of them (a HTTP server,
/// for example), regardless of what stream is used underneath.
pub trait ResourceMaker<O, C, PipeThrough>
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    Self: Sized,
{
    /// The kind of resource the low-level thing builds.
    ///
    /// This is, for example, the TCP stream.
    type Resource;

    /// Extra custom configuration carried in the config fragment.
    ///
    /// This allows some data to be passed from the configuration to the provided action without
    /// interference by the maker. If not used, it can be ().
    type ExtraCfg: Clone + Debug + PartialEq + Send + 'static;

    /// Installs the pipeline.
    ///
    /// Similarly to helpers, this takes all the needed pieces and installs the pipeline to extract
    /// part of configuration, turn it into a resource, apply the user action and spawn the
    /// returned future into the builder.
    ///
    /// # Params
    ///
    /// * `extractor`: Closure that extracts a fragment from the configuration. This describe how
    ///   the resource is created.
    /// * `action`: Something the user wants to be done with each created resource.
    /// * `name`: How the thing is called in the logs.
    /// * `builder`: The builder to modify.
    fn apply<Extractor, ExtractedIter, Action, ActionFut, Name>(
        extractor: Extractor,
        action: Action,
        name: Name,
        builder: Builder<O, C>,
    ) -> Builder<O, C>
    where
        Extractor: FnMut(&C) -> ExtractedIter + Send + 'static,
        ExtractedIter: IntoIterator<Item = (Self, PipeThrough)>,
        Action: Fn(&Arc<Spirit<O, C>>, Self::Resource, &Self::ExtraCfg, &PipeThrough) -> ActionFut
            + Send
            + Sync
            + 'static,
        ActionFut: IntoFuture<Item = (), Error = Error>,
        ActionFut::Future: Send + 'static,
        Name: Clone + Display + Send + Sync + 'static;
}

/// A configuration fragment of a TCP listening socket.
///
/// This describes a TCP listening socket. It works as an iterated configuration helper ‒ if you
/// extract a vector of these from configuration and an action to handle one TCP connection, it
/// manages the listening itself when attached to spirit.
///
/// # Type parameters
///
/// * `ExtraCfg`: Any additional configuration options, passed to the action callback. Defaults to
///   an empty set of parameters.
/// * `ScaleMode`: A description of how to scale into multiple listening instances.
///
/// # Configuration options
///
/// Aside from the options from the type parameters above, these options are present:
///
/// * `host`: The host/interface to listen on, defaults to `*`.
/// * `port`: Mandatory, the port to listen to.
/// * `error_sleep_ms`: If there's a recoverable error like „Too many open files“, this many
///   milliseconds is waited before trying to accept more connections. Defaults to 100.
/// * `max_conn`: Maximum number of parallel connections. This is per one instance, therefore the
///   total number of connections being handled is `scale * max_conn` (if scaling is enabled).
///   Defaults to 1000.
///
/// # Example
///
/// TODO (adjust the one from the crate level config)
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TcpListen<ExtraCfg = Empty, ScaleMode: Scaled = Scale> {
    #[serde(flatten)]
    listen: Listen,
    #[serde(flatten)]
    scale: ScaleMode,
    #[serde(rename = "error-sleep-ms", default = "default_error_sleep")]
    error_sleep_ms: u64,
    #[serde(rename = "max-conn", default = "default_max_conn")]
    max_conn: usize,
    #[serde(flatten)]
    extra_cfg: ExtraCfg,
}

impl<ExtraCfg, ScaleMode> TcpListen<ExtraCfg, ScaleMode>
where
    ExtraCfg: Clone + Debug + PartialEq + Send + 'static,
    ScaleMode: Scaled,
{
    /// Provides a helper for this configuration.
    ///
    /// While you are free to use this directly, it is more commonly used through
    /// `spirit::Builder::config_helper` with an extractor returning iterator of this type.
    ///
    /// # Parameters
    ///
    /// * `extract`: Closure that extracts an iterator of `TcpListen` out of the whole
    ///   configuration.
    /// * `conn`: An action to be taken on each accepted connection.
    /// * `name`: How to call the instances in logs.
    pub fn helper<Extract, ExtractIt, Conn, ConnFut, Name, O, C>(
        mut extract: Extract,
        conn: Conn,
        name: Name,
    ) -> impl Helper<O, C>
    where
        C: DeserializeOwned + Send + Sync + 'static,
        O: Debug + StructOpt + Sync + Send + 'static,
        Extract: FnMut(&C) -> ExtractIt + Send + 'static,
        ExtractIt: IntoIterator<Item = Self>,
        Conn: Fn(&Arc<Spirit<O, C>>, TcpStream, &ExtraCfg) -> ConnFut + Sync + Send + 'static,
        ConnFut: IntoFuture<Item = (), Error = Error>,
        ConnFut::Future: Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        let conn = Arc::new(conn);

        let to_task_name = name.clone();
        let to_task =
            move |spirit: &Arc<Spirit<O, C>>,
                  listener: Arc<StdTcpListener>,
                  (cfg, error_sleep, max_conn): (ExtraCfg, Duration, usize)| {
                let spirit = Arc::clone(spirit);
                let conn = Arc::clone(&conn);
                let name = to_task_name.clone();
                listener
                    .try_clone() // Another copy of the listener
                    // std → tokio socket conversion
                    .and_then(|listener| TcpListener::from_std(listener, &Handle::default()))
                    .into_future()
                    .and_then(move |listener| {
                        listener
                            .incoming()
                            // Handle errors like too many open FDs gracefully
                            .sleep_on_error(error_sleep)
                            .map(move |new_conn| {
                                let name = name.clone();
                                // The listen below keeps track of how many parallel connections
                                // there are. But it does so inside the same future, which prevents
                                // the separate connections to be handled in parallel on a thread
                                // pool. So we spawn the future to handle the connection itself.
                                // But we want to keep the future alive so the listen doesn't think
                                // it already terminated, therefore the done-channel.
                                let (done_send, done_recv) = oneshot::channel();
                                let handle_conn =
                                    conn(&spirit, new_conn, &cfg).into_future().then(move |r| {
                                        if let Err(e) = r {
                                            error!(
                                                "Failed to handle connection on {}: {}",
                                                name, e
                                            );
                                        }
                                        // Ignore the other side going away. This may happen if the
                                        // listener terminated, but the connection lingers for
                                        // longer.
                                        let _ = done_send.send(());
                                        future::ok(())
                                    });
                                tokio::spawn(handle_conn);
                                done_recv.then(|_| future::ok(()))
                            })
                            .listen(max_conn)
                            .map_err(|()| unreachable!("tk-listen never errors"))
                    })
                    .map_err(Error::from)
            };

        let extract_name = name.clone();
        let extract = move |cfg: &C| {
            extract(cfg).into_iter().map(|c| {
                let (scale, results) = c.scale.scaled(&extract_name);
                let sleep = Duration::from_millis(c.error_sleep_ms);
                (c.listen, (c.extra_cfg, sleep, c.max_conn), scale, results)
            })
        };

        Task {
            extract,
            build: |cfg: &Listen| cfg.create_tcp().map(Arc::new),
            to_task,
            name,
        }
    }
}

impl<ExtraCfg, ScaleMode> ExtraCfgCarrier for TcpListen<ExtraCfg, ScaleMode>
where
    ScaleMode: Scaled,
{
    type Extra = ExtraCfg;
    fn extra(&self) -> &ExtraCfg {
        &self.extra_cfg
    }
}

impl<ExtraCfg, ScaleMode, O, C> ResourceConfig<O, C> for TcpListen<ExtraCfg, ScaleMode>
where
    ExtraCfg: Debug + PartialEq + Send + Sync + 'static,
    ScaleMode: Debug + PartialEq + Scaled + Send + Sync + 'static,
{
    type Seed = StdTcpListener;
    type Resource = TcpListener;
    fn create(&self, _: &str) -> Result<StdTcpListener, Error> {
        self.listen.create_tcp()
    }
    fn fork(&self, seed: &StdTcpListener, _: &str) -> Result<TcpListener, Error> {
        seed.try_clone() // Another copy of the listener
            // std → tokio socket conversion
            .and_then(|listener| TcpListener::from_std(listener, &Handle::default()))
            .map_err(Error::from)
    }
    fn scaled(&self, name: &str) -> (usize, ValidationResults) {
        self.scale.scaled(name)
    }
    fn is_similar(&self, other: &Self, _: &str) -> bool {
        self.listen == other.listen
    }
}

impl<ExtraCfg: Default, ScaleMode: Default + Scaled> Default for TcpListen<ExtraCfg, ScaleMode> {
    fn default() -> Self {
        Self {
            listen: Listen::default(),
            scale: ScaleMode::default(),
            error_sleep_ms: default_error_sleep(),
            max_conn: default_max_conn(),
            extra_cfg: ExtraCfg::default(),
        }
    }
}

impl<O, C, Conn, ConnFut, ExtraCfg, ScaleMode> IteratedCfgHelper<O, C, Conn>
    for TcpListen<ExtraCfg, ScaleMode>
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    ExtraCfg: Clone + Debug + PartialEq + Send + 'static,
    Conn: Fn(&Arc<Spirit<O, C>>, TcpStream, &ExtraCfg) -> ConnFut + Sync + Send + 'static,
    ConnFut: IntoFuture<Item = (), Error = Error>,
    ConnFut::Future: Send + 'static,
    ScaleMode: Scaled,
{
    fn apply<Extractor, ExtractedIter, Name>(
        extractor: Extractor,
        action: Conn,
        name: Name,
        builder: Builder<O, C>,
    ) -> Builder<O, C>
    where
        Extractor: FnMut(&C) -> ExtractedIter + Send + 'static,
        ExtractedIter: IntoIterator<Item = Self>,
        Name: Clone + Display + Send + Sync + 'static,
    {
        Self::helper(extractor, action, name).apply(builder)
    }
}

impl<O, C, ExtraCfg, PipeThrough, ScaleMode> ResourceMaker<O, C, PipeThrough>
    for TcpListen<ExtraCfg, ScaleMode>
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    ExtraCfg: Clone + Debug + PartialEq + Send + 'static,
    PipeThrough: Clone + Debug + PartialEq + Send + 'static,
    ScaleMode: Scaled,
{
    type Resource = TcpStream;
    type ExtraCfg = ExtraCfg;
    fn apply<Extractor, ExtractedIter, Action, ActionFut, Name>(
        mut extractor: Extractor,
        action: Action,
        name: Name,
        builder: Builder<O, C>,
    ) -> Builder<O, C>
    where
        Extractor: FnMut(&C) -> ExtractedIter + Send + 'static,
        ExtractedIter: IntoIterator<Item = (Self, PipeThrough)>,
        Action: Fn(&Arc<Spirit<O, C>>, TcpStream, &Self::ExtraCfg, &PipeThrough) -> ActionFut
            + Send
            + Sync
            + 'static,
        ActionFut: IntoFuture<Item = (), Error = Error>,
        ActionFut::Future: Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        let extractor = move |cfg: &_| {
            extractor(cfg)
                .into_iter()
                .map(|(tcp, pipe_through)| TcpListen {
                    listen: tcp.listen,
                    scale: tcp.scale,
                    error_sleep_ms: tcp.error_sleep_ms,
                    max_conn: tcp.max_conn,
                    extra_cfg: (tcp.extra_cfg, pipe_through),
                })
        };
        let action = move |spirit: &_, stream, extra_cfg: &(_, _)| {
            action(spirit, stream, &extra_cfg.0, &extra_cfg.1)
        };
        TcpListen::<(ExtraCfg, PipeThrough), ScaleMode>::helper(extractor, action, name)
            .apply(builder)
    }
}

impl<O, C, Conn, ConnFut, ExtraCfg, ScaleMode> CfgHelper<O, C, Conn>
    for TcpListen<ExtraCfg, ScaleMode>
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    ExtraCfg: Clone + Debug + PartialEq + Send + 'static,
    Conn: Fn(&Arc<Spirit<O, C>>, TcpStream, &ExtraCfg) -> ConnFut + Sync + Send + 'static,
    ConnFut: IntoFuture<Item = (), Error = Error> + Send + 'static,
    ConnFut::Future: Send + 'static,
    ScaleMode: Scaled,
{
    fn apply<Extractor, Name>(
        mut extractor: Extractor,
        action: Conn,
        name: Name,
        builder: Builder<O, C>,
    ) -> Builder<O, C>
    where
        Extractor: FnMut(&C) -> Self + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        let extractor = move |cfg: &_| iter::once(extractor(cfg));
        Self::helper(extractor, action, name).apply(builder)
    }
}

/// A configuration fragment describing a bound UDP socket.
///
/// This is similar to [`TcpListen`](struct.TcpListen.html), but for UDP sockets. While the action
/// on a TCP socket is called for each new accepted connection, the action for UDP socket is used
/// to handle the whole UDP socket created from this configuration.
///
/// # Type parameters
///
/// * `ExtraCfg`: Extra options folded into this configuration, for application specific options.
///   They are passed to the action.
/// * `ScaleMode`: How scaling should be done. If scaling is enabled, the action should handle
///   situation where it runs in multiple instances. However, even in case scaling is disabled, the
///   action needs to handle being „restarted“ ‒ if there's a new configuration for the socket, the
///   old future is dropped and new one, with a new socket, is created.
///
/// # Configuration options
///
/// In addition to options provided by the above type parameters, these are present:
///
/// * `host`: The hostname/interface to bind to. Defaults to `*`.
/// * `port`: The port to bind the UDP socket to (mandatory). While it is possible to create
///   unbound UDP sockets with an OS-assigned port, these don't need the configuration and are not
///   created by this configuration fragment.
///
/// #
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct UdpListen<ExtraCfg = Empty, ScaleMode: Scaled = Scale> {
    #[serde(flatten)]
    listen: Listen,
    #[serde(flatten)]
    scale: ScaleMode,
    #[serde(flatten)]
    extra_cfg: ExtraCfg,
}

impl<ExtraCfg, ScaleMode> UdpListen<ExtraCfg, ScaleMode>
where
    ExtraCfg: Clone + Debug + PartialEq + Send + 'static,
    ScaleMode: Scaled,
{
    /// Returns a helper for handling reconfiguration of the UDP sockets.
    ///
    /// While it can be used manually, it is usually used through the
    /// `spirit::Builder::config_helper` with extractor returning an iterator of `UdpListen`.
    pub fn helper<Extract, ExtractIt, Action, Fut, Name, O, C>(
        mut extract: Extract,
        action: Action,
        name: Name,
    ) -> impl Helper<O, C>
    where
        C: DeserializeOwned + Send + Sync + 'static,
        O: Debug + StructOpt + Sync + Send + 'static,
        Extract: FnMut(&C) -> ExtractIt + Send + 'static,
        ExtractIt: IntoIterator<Item = Self>,
        Action: Fn(&Arc<Spirit<O, C>>, UdpSocket, &ExtraCfg) -> Fut + Sync + Send + 'static,
        Fut: Future<Item = (), Error = Error> + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        trace!("Creating UDP listen helper for {}", name);
        let action = Arc::new(action);

        let to_task_name = name.clone();
        let to_task =
            move |spirit: &Arc<Spirit<O, C>>, socket: Arc<StdUdpSocket>, cfg: ExtraCfg| {
                trace!("Running UDP listener {} for {:?}", to_task_name, cfg);
                let spirit = Arc::clone(spirit);
                let action = Arc::clone(&action);
                socket
                    .try_clone() // Another copy of the listener
                    // std → tokio socket conversion
                    .and_then(|socket| UdpSocket::from_std(socket, &Handle::default()))
                    .map_err(Error::from)
                    .into_future()
                    .and_then(move |socket| action(&spirit, socket, &cfg))
            };

        let extract_name = name.clone();
        let extract = move |cfg: &C| {
            trace!("Extracting {}", extract_name);
            extract(cfg).into_iter().map(|c| {
                let (scale, results) = c.scale.scaled(&extract_name);
                (c.listen, c.extra_cfg, scale, results)
            })
        };

        Task {
            extract,
            build: |listen: &Listen| listen.create_udp().map(Arc::new),
            to_task,
            name,
        }
    }
}

impl<ExtraCfg, ScaleMode> ExtraCfgCarrier for UdpListen<ExtraCfg, ScaleMode>
where
    ScaleMode: Scaled,
{
    type Extra = ExtraCfg;
    fn extra(&self) -> &ExtraCfg {
        &self.extra_cfg
    }
}

impl<ExtraCfg, ScaleMode, O, C> ResourceConfig<O, C> for UdpListen<ExtraCfg, ScaleMode>
where
    ExtraCfg: Debug + PartialEq + Send + Sync + 'static,
    ScaleMode: Debug + PartialEq + Scaled + Send + Sync + 'static,
{
    type Seed = StdUdpSocket;
    type Resource = UdpSocket;
    fn create(&self, _: &str) -> Result<StdUdpSocket, Error> {
        self.listen.create_udp()
    }
    fn fork(&self, seed: &StdUdpSocket, _: &str) -> Result<UdpSocket, Error> {
        seed.try_clone() // Another copy of the listener
            // std → tokio socket conversion
            .and_then(|listener| UdpSocket::from_std(listener, &Handle::default()))
            .map_err(Error::from)
    }
    fn scaled(&self, name: &str) -> (usize, ValidationResults) {
        self.scale.scaled(name)
    }
    fn is_similar(&self, other: &Self, _: &str) -> bool {
        self.listen == other.listen
    }
}

impl<O, C, Action, Fut, ExtraCfg, ScaleMode> IteratedCfgHelper<O, C, Action>
    for UdpListen<ExtraCfg, ScaleMode>
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    ExtraCfg: Clone + Debug + PartialEq + Send + 'static,
    Action: Fn(&Arc<Spirit<O, C>>, UdpSocket, &ExtraCfg) -> Fut + Sync + Send + 'static,
    Fut: Future<Item = (), Error = Error> + Send + 'static,
    ScaleMode: Scaled,
{
    fn apply<Extractor, ExtractedIter, Name>(
        extractor: Extractor,
        action: Action,
        name: Name,
        builder: Builder<O, C>,
    ) -> Builder<O, C>
    where
        Extractor: FnMut(&C) -> ExtractedIter + Send + 'static,
        ExtractedIter: IntoIterator<Item = Self>,
        Name: Clone + Display + Send + Sync + 'static,
    {
        Self::helper(extractor, action, name).apply(builder)
    }
}

impl<O, C, Action, Fut, ExtraCfg, ScaleMode> CfgHelper<O, C, Action>
    for UdpListen<ExtraCfg, ScaleMode>
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    ExtraCfg: Clone + Debug + PartialEq + Send + 'static,
    Action: Fn(&Arc<Spirit<O, C>>, UdpSocket, &ExtraCfg) -> Fut + Sync + Send + 'static,
    Fut: Future<Item = (), Error = Error> + Send + 'static,
    ScaleMode: Scaled,
{
    fn apply<Extractor, Name>(
        mut extractor: Extractor,
        action: Action,
        name: Name,
        builder: Builder<O, C>,
    ) -> Builder<O, C>
    where
        Extractor: FnMut(&C) -> Self + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        let extractor = move |cfg: &_| iter::once(extractor(cfg));
        Self::helper(extractor, action, name).apply(builder)
    }
}

/// A body run on tokio runtime.
///
/// When specifying custom tokio runtime through the [`Runtime`](enum.Runtime.html) helper, this is
/// the future to be run inside the runtime.
pub type TokioBody = Box<Future<Item = (), Error = Error> + Send>;

/// A helper to initialize a tokio runtime as part of spirit.
///
/// The helpers in this crate ([`TcpListen`](struct.TcpListen.html),
/// [`UdpListen`](struct.UdpListen.html)) use this to make sure they have a runtime to handle the
/// sockets on.
///
/// If you prefer to specify configuration of the runtime to use, instead of the default one, you
/// can create an instance of this helper yourself and register it *before registering any socket
/// helpers*, which will take precedence and the sockets will use the one provided by you.
///
/// Note that the provided closures are `FnMut` mostly because `Box<FnOnce>` doesn't work. They
/// will be called just once, so you can use `Option<T>` inside and consume the value by
/// `take.unwrap()`.
///
/// # Future compatibility
///
/// More options may be added into the enum at any time. Such change will not be considered a
/// breaking change.
///
/// # Examples
///
/// TODO: Something with registering it first and registering another helper later on.
pub enum Runtime {
    /// Use the threadpool runtime.
    ///
    /// The threadpool runtime is the default (both in tokio and spirit).
    ///
    /// This allows you to modify the builder prior to starting it, specifying custom options.
    ThreadPool(Box<FnMut(&mut runtime::Builder) + Send>),
    /// Use the current thread runtime.
    ///
    /// If you prefer to run everything in a single thread, use this variant. The provided closure
    /// can modify the builder prior to starting it.
    CurrentThread(Box<FnMut(&mut runtime::current_thread::Builder) + Send>),
    /// Use completely custom runtime.
    ///
    /// The provided closure should start the runtime and execute the provided future on it,
    /// blocking until the runtime becomes empty.
    ///
    /// This allows combining arbitrary runtimes that are not directly supported by either tokio or
    /// spirit.
    Custom(Box<FnMut(TokioBody) -> Result<(), Error> + Send>),
    #[doc(hidden)]
    __NonExhaustive__,
    // TODO: Support loading this from configuration? But it won't be possible to modify at
    // runtime, will it?
}

impl Default for Runtime {
    fn default() -> Self {
        Runtime::ThreadPool(Box::new(|_| {}))
    }
}

impl<O, C> Helper<O, C> for Runtime
where
    C: DeserializeOwned + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
{
    fn apply(self, builder: Builder<O, C>) -> Builder<O, C> {
        trace!("Wrapping in tokio runtime");
        builder.body_wrapper(|_spirit, inner| {
            let fut = future::lazy(move || inner.run());
            match self {
                Runtime::ThreadPool(mut mod_builder) => {
                    let mut builder = runtime::Builder::new();
                    mod_builder(&mut builder);
                    builder.build()?.block_on_all(fut)
                }
                Runtime::CurrentThread(mut mod_builder) => {
                    let mut builder = runtime::current_thread::Builder::new();
                    mod_builder(&mut builder);
                    let mut runtime = builder.build()?;
                    let result = runtime.block_on(fut);
                    runtime.run()?;
                    result
                }
                Runtime::Custom(mut callback) => callback(Box::new(fut)),
                Runtime::__NonExhaustive__ => unreachable!(),
            }
        })
    }
}
