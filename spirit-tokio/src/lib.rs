#![doc(
    html_root_url = "https://docs.rs/spirit-tokio/0.1.0/spirit_tokio/",
    test(attr(deny(warnings)))
)]
#![cfg_attr(feature = "cargo-clippy", allow(type_complexity))]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

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

use std::borrow::Borrow;
use std::fmt::{Debug, Display};
use std::iter;
use std::net::{TcpListener as StdTcpListener, UdpSocket as StdUdpSocket};
use std::sync::Arc;
use std::time::Duration;

use failure::Error;
use futures::sync::{mpsc, oneshot};
use futures::Future;
use parking_lot::Mutex;
use serde::Deserialize;
use spirit::helpers::{CfgHelper, Helper, IteratedCfgHelper};
use spirit::validation::{Result as ValidationResult, Results as ValidationResults};
use spirit::{ArcSwap, Builder, Empty, Spirit};
use structopt::StructOpt;
use tk_listen::ListenExt;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::prelude::*;
use tokio::reactor::Handle;
use tokio::runtime::Runtime as TokioRuntime;

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

pub struct Task<Extract, Build, ToTask, Name> {
    pub extract: Extract,
    pub build: Build,
    pub to_task: ToTask,
    pub name: Name,
}

impl<S, O, C, SubCfg, Resource, Extract, ExtractIt, ExtraCfg, Build, ToTask, InnerTask, Name>
    Helper<S, O, C> for Task<Extract, Build, ToTask, Name>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    Extract: FnMut(&C) -> ExtractIt + Send + 'static,
    ExtractIt: IntoIterator<Item = (SubCfg, ExtraCfg, usize, ValidationResults)>,
    ExtraCfg: Clone + Debug + PartialEq + Send + 'static,
    SubCfg: Clone + Debug + PartialEq + Send + 'static,
    Build: FnMut(&SubCfg) -> Result<Resource, Error> + Send + 'static,
    Resource: Clone + Send + 'static,
    ToTask: FnMut(&Arc<Spirit<S, O, C>>, Resource, ExtraCfg) -> InnerTask + Send + 'static,
    InnerTask: IntoFuture<Item = (), Error = Error> + Send + 'static,
    <InnerTask as IntoFuture>::Future: Send,
    Name: Clone + Display + Send + Sync + 'static,
{
    fn apply(self, builder: Builder<S, O, C>) -> Builder<S, O, C> {
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
        let installer = move |spirit: &Arc<Spirit<S, O, C>>| {
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
                            let msg = format!("Creationg of {} for {:?} failed: {}", name, sub, e);
                            debug!("{}", msg); // The error will appear together for the validator
                            results.merge(ValidationResult::error(msg));
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
            .with_singleton(Runtime)
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

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Listen {
    port: u16,
    #[serde(default = "default_host")]
    host: String,
}

impl Listen {
    pub fn create_tcp(&self) -> Result<Arc<StdTcpListener>, Error> {
        Ok(Arc::new(StdTcpListener::bind((
            &self.host as &str,
            self.port,
        ))?))
    }
    pub fn create_udp(&self) -> Result<Arc<StdUdpSocket>, Error> {
        Ok(Arc::new(StdUdpSocket::bind((
            &self.host as &str,
            self.port,
        ))?))
    }
}

fn default_error_sleep() -> u64 {
    100
}

fn default_max_conn() -> usize {
    1000
}

pub trait Scaled {
    fn scaled<Name: Display>(&self, name: &Name) -> (usize, ValidationResults);
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Scale {
    #[serde(default = "default_scale")]
    scale: usize,
}

impl Scaled for Scale {
    fn scaled<Name: Display>(&self, name: &Name) -> (usize, ValidationResults) {
        if self.scale > 0 {
            (self.scale, ValidationResults::new())
        } else {
            let msg = format!("Turning scale in {} from 0 to 1", name);
            (1, ValidationResult::warning(msg).into())
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Singleton {}

impl Scaled for Singleton {
    fn scaled<Name: Display>(&self, _: &Name) -> (usize, ValidationResults) {
        (1, ValidationResults::new())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
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

impl<ExtraCfg: Clone + Debug + PartialEq + Send + 'static> TcpListen<ExtraCfg> {
    pub fn helper<Extract, ExtractIt, Conn, ConnFut, Name, S, O, C>(
        mut extract: Extract,
        conn: Conn,
        name: Name,
    ) -> impl Helper<S, O, C>
    where
        S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
        for<'de> C: Deserialize<'de> + Send + Sync + 'static,
        O: Debug + StructOpt + Sync + Send + 'static,
        Extract: FnMut(&C) -> ExtractIt + Send + 'static,
        ExtractIt: IntoIterator<Item = Self>,
        Conn: Fn(&Arc<Spirit<S, O, C>>, TcpStream, &ExtraCfg) -> ConnFut + Sync + Send + 'static,
        ConnFut: Future<Item = (), Error = Error> + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        let conn = Arc::new(conn);

        let to_task_name = name.clone();
        let to_task =
            move |spirit: &Arc<Spirit<S, O, C>>,
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
                        listener.incoming()
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
                                let handle_conn = conn(&spirit, new_conn, &cfg)
                                    .then(move |r| {
                                        if let Err(e) = r {
                                            error!("Failed to handle connection on {}: {}", name, e);
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
                    }).map_err(Error::from)
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
            build: Listen::create_tcp,
            to_task,
            name,
        }
    }
}

impl<S, O, C, Conn, ConnFut, ExtraCfg> IteratedCfgHelper<S, O, C, Conn> for TcpListen<ExtraCfg>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    ExtraCfg: Clone + Debug + PartialEq + Send + 'static,
    Conn: Fn(&Arc<Spirit<S, O, C>>, TcpStream, &ExtraCfg) -> ConnFut + Sync + Send + 'static,
    ConnFut: Future<Item = (), Error = Error> + Send + 'static,
{
    fn apply<Extractor, ExtractedIter, Name>(
        extractor: Extractor,
        action: Conn,
        name: Name,
        builder: Builder<S, O, C>,
    ) -> Builder<S, O, C>
    where
        Extractor: FnMut(&C) -> ExtractedIter + Send + 'static,
        ExtractedIter: IntoIterator<Item = Self>,
        Name: Clone + Display + Send + Sync + 'static,
    {
        Self::helper(extractor, action, name).apply(builder)
    }
}

impl<S, O, C, Conn, ConnFut, ExtraCfg> CfgHelper<S, O, C, Conn> for TcpListen<ExtraCfg>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    ExtraCfg: Clone + Debug + PartialEq + Send + 'static,
    Conn: Fn(&Arc<Spirit<S, O, C>>, TcpStream, &ExtraCfg) -> ConnFut + Sync + Send + 'static,
    ConnFut: Future<Item = (), Error = Error> + Send + 'static,
{
    fn apply<Extractor, Name>(
        mut extractor: Extractor,
        action: Conn,
        name: Name,
        builder: Builder<S, O, C>,
    ) -> Builder<S, O, C>
    where
        Extractor: FnMut(&C) -> Self + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        let extractor = move |cfg: &_| iter::once(extractor(cfg));
        Self::helper(extractor, action, name).apply(builder)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct UdpListen<ExtraCfg = Empty, ScaleMode: Scaled = Scale> {
    #[serde(flatten)]
    listen: Listen,
    #[serde(flatten)]
    scale: ScaleMode,
    #[serde(flatten)]
    extra_cfg: ExtraCfg,
}

impl<ExtraCfg: Clone + Debug + PartialEq + Send + 'static> UdpListen<ExtraCfg> {
    pub fn helper<Extract, ExtractIt, Action, Fut, Name, S, O, C>(
        mut extract: Extract,
        action: Action,
        name: Name,
    ) -> impl Helper<S, O, C>
    where
        S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
        for<'de> C: Deserialize<'de> + Send + Sync + 'static,
        O: Debug + StructOpt + Sync + Send + 'static,
        Extract: FnMut(&C) -> ExtractIt + Send + 'static,
        ExtractIt: IntoIterator<Item = Self>,
        Action: Fn(&Arc<Spirit<S, O, C>>, UdpSocket, &ExtraCfg) -> Fut + Sync + Send + 'static,
        Fut: Future<Item = (), Error = Error> + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        trace!("Creating UDP listen helper for {}", name);
        let action = Arc::new(action);

        let to_task_name = name.clone();
        let to_task =
            move |spirit: &Arc<Spirit<S, O, C>>, socket: Arc<StdUdpSocket>, cfg: ExtraCfg| {
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
            build: Listen::create_udp,
            to_task,
            name,
        }
    }
}

impl<S, O, C, Action, Fut, ExtraCfg> IteratedCfgHelper<S, O, C, Action> for UdpListen<ExtraCfg>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    ExtraCfg: Clone + Debug + PartialEq + Send + 'static,
    Action: Fn(&Arc<Spirit<S, O, C>>, UdpSocket, &ExtraCfg) -> Fut + Sync + Send + 'static,
    Fut: Future<Item = (), Error = Error> + Send + 'static,
{
    fn apply<Extractor, ExtractedIter, Name>(
        extractor: Extractor,
        action: Action,
        name: Name,
        builder: Builder<S, O, C>,
    ) -> Builder<S, O, C>
    where
        Extractor: FnMut(&C) -> ExtractedIter + Send + 'static,
        ExtractedIter: IntoIterator<Item = Self>,
        Name: Clone + Display + Send + Sync + 'static,
    {
        Self::helper(extractor, action, name).apply(builder)
    }
}

impl<S, O, C, Action, Fut, ExtraCfg> CfgHelper<S, O, C, Action> for UdpListen<ExtraCfg>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    ExtraCfg: Clone + Debug + PartialEq + Send + 'static,
    Action: Fn(&Arc<Spirit<S, O, C>>, UdpSocket, &ExtraCfg) -> Fut + Sync + Send + 'static,
    Fut: Future<Item = (), Error = Error> + Send + 'static,
{
    fn apply<Extractor, Name>(
        mut extractor: Extractor,
        action: Action,
        name: Name,
        builder: Builder<S, O, C>,
    ) -> Builder<S, O, C>
    where
        Extractor: FnMut(&C) -> Self + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        let extractor = move |cfg: &_| iter::once(extractor(cfg));
        Self::helper(extractor, action, name).apply(builder)
    }
}

pub struct Runtime;

impl<S, O, C> Helper<S, O, C> for Runtime
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
{
    fn apply(self, builder: Builder<S, O, C>) -> Builder<S, O, C> {
        trace!("Wrapping in tokio runtime");
        builder.body_wrapper(|_spirit, inner| {
            TokioRuntime::new()?.block_on_all(future::lazy(move || inner.run()))
        })
    }
}
