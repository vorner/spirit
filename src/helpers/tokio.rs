use std::borrow::Borrow;
use std::collections::HashMap;
use std::fmt::{Debug, Display};
use std::hash::Hash;
use std::marker::PhantomData;
use std::mem;
use std::net::TcpListener as StdTcpListener;
use std::sync::Arc;

use arc_swap::ArcSwap;
use failure::Error;
use futures::sync::{mpsc, oneshot};
use futures::Future;
use parking_lot::Mutex;
use serde::Deserialize;
use structopt::StructOpt;
use tokio;
use tokio::net::{TcpListener, TcpStream};
use tokio::prelude::*;
use tokio::reactor::Handle;

use super::super::validation::Result as ValidationResult;
use super::super::{Builder, Spirit, ValidationResults};
use super::Helper;

type BoxTask = Box<dyn Future<Item = (), Error = ()> + Send>;
// Should be Box<FnOnce>, but it doesn't work. So we wrap Option<FnOnce> into FnMut, pull it out
// and make sure not to call it twice (which would panic).
type TaskBuilder<T> = Box<dyn FnMut(&T) -> BoxTask + Send>;

pub(crate) struct TokioGutsInner<T> {
    tasks: Vec<TaskBuilder<T>>,
}

// TODO: Why can't derive?
impl<T> Default for TokioGutsInner<T> {
    fn default() -> Self {
        TokioGutsInner { tasks: Vec::new() }
    }
}

pub(crate) struct TokioGuts<T>(Mutex<TokioGutsInner<T>>);

impl<T> From<TokioGutsInner<T>> for TokioGuts<T> {
    fn from(inner: TokioGutsInner<T>) -> Self {
        TokioGuts(Mutex::new(inner))
    }
}

impl<S, O, C> Spirit<S, O, C>
where
    S: Borrow<ArcSwap<C>> + Send + Sync + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync,
    O: StructOpt,
{
    pub fn tokio_spawn_tasks(me: &Arc<Self>) {
        let mut extracted = Vec::new();
        mem::swap(&mut extracted, &mut me.tokio_guts.0.lock().tasks);
        for mut task in extracted.drain(..) {
            tokio::spawn(task(me));
        }
    }
}

impl<S, O, C> Builder<S, O, C>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
{
    pub fn tokio_task<F, Fut>(mut self, task: F) -> Self
    where
        F: FnOnce(&Arc<Spirit<S, O, C>>) -> Fut + Send + 'static,
        Fut: Future<Item = (), Error = ()> + Send + 'static,
    {
        let mut task = Some(task);
        // We effectively simulate FnOnce by FnMut that'd panic on another call
        let wrapper = move |spirit: &_| -> BoxTask {
            let task = task.take().unwrap();
            let fut = task(spirit);
            Box::new(fut)
        };
        self.tokio_guts.tasks.push(Box::new(wrapper));
        self
    }

    pub fn run_tokio(self) {
        self.run(|spirit| -> Result<(), Error> {
            tokio::run(future::lazy(move || {
                Spirit::tokio_spawn_tasks(&spirit);
                future::ok(())
            }));
            Ok(())
        })
    }
}

struct TokioApply<B, F: FnOnce(B) -> B>(F, PhantomData<fn(B)>);

impl<S, O, C, F> Helper<S, O, C> for TokioApply<Builder<S, O, C>, F>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    F: FnOnce(Builder<S, O, C>) -> Builder<S, O, C>,
{
    fn apply(self, builder: Builder<S, O, C>) -> Builder<S, O, C> {
        (self.0)(builder)
    }
}

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

impl Task<(), (), (), ()> {
    pub fn new() -> Self {
        Task {
            extract: (),
            build: (),
            to_task: (),
            name: (),
        }
    }
}

impl<Extract, Build, ToTask, Name> Task<Extract, Build, ToTask, Name> {
    pub fn with_extract<NewExtract>(
        self,
        extract: NewExtract,
    ) -> Task<NewExtract, Build, ToTask, Name> {
        Task {
            extract: extract,
            build: self.build,
            to_task: self.to_task,
            name: self.name,
        }
    }
    pub fn with_build<NewBuild>(self, build: NewBuild) -> Task<Extract, NewBuild, ToTask, Name> {
        Task {
            extract: self.extract,
            build: build,
            to_task: self.to_task,
            name: self.name,
        }
    }
    pub fn with_to_task<NewToTask>(
        self,
        to_task: NewToTask,
    ) -> Task<Extract, Build, NewToTask, Name> {
        Task {
            extract: self.extract,
            build: self.build,
            to_task: to_task,
            name: self.name,
        }
    }
    pub fn with_name<NewName>(self, name: NewName) -> Task<Extract, Build, ToTask, NewName> {
        Task {
            extract: self.extract,
            build: self.build,
            to_task: self.to_task,
            name: name,
        }
    }
}

impl<S, O, C, SubCfg, Resource, Extract, ExtractIt, Build, ToTask, InnerTask, Name> Helper<S, O, C>
    for Task<Extract, Build, ToTask, Name>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    Extract: FnMut(&C) -> ExtractIt + Send + 'static,
    ExtractIt: IntoIterator<Item = SubCfg>,
    SubCfg: PartialEq + Debug + Send + 'static,
    Build: FnMut(&SubCfg) -> Result<Resource, Error> + Send + 'static,
    Resource: Send + 'static,
    ToTask: FnMut(&Arc<Spirit<S, O, C>>, Resource) -> InnerTask + Send + 'static,
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
        struct Install<R> {
            resource: R,
            drop_req: oneshot::Receiver<()>,
            confirm_drop: oneshot::Sender<()>,
            cfg: String,
        }
        let (install_sender, install_receiver) = mpsc::unbounded::<Install<Resource>>();

        let installer_name = name.clone();
        let installer = move |spirit: &Arc<Spirit<S, O, C>>| {
            let spirit = Arc::clone(spirit);
            install_receiver.for_each(move |install| {
                let Install {
                    resource,
                    drop_req,
                    confirm_drop,
                    cfg,
                } = install;
                let name = installer_name.clone();
                debug!("Installing resource {} with config {}", name, cfg);
                // Get the task itself
                let task = to_task(&spirit, resource).into_future();
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

        let cache = Arc::new(Mutex::new(Vec::<(SubCfg, Arc<RemoteDrop>)>::new()));
        let validator = move |_: &Arc<C>, cfg: &mut C, _: &O| -> ValidationResults {
            let mut results = ValidationResults::new();
            let orig_cache = cache.lock();
            let mut new_cache = Vec::<(SubCfg, Arc<RemoteDrop>)>::new();
            let mut to_send = Vec::new();
            for sub in extract(cfg) {
                let previous = orig_cache
                    .iter()
                    .find(|(cfg, _)| cfg == &sub)
                    .map(|(_, remote)| Arc::clone(remote));
                if let Some(previous) = previous {
                    debug!("Reusing previous instance of {} for {:?}", name, sub);
                    new_cache.push((sub, previous));
                } else {
                    trace!("Creating new instance of {} for {:?}", name, sub);
                    match build(&sub) {
                        Ok(resource) => {
                            debug!("Successfully created instance of {} for {:?}", name, sub);
                            let (req_sender, req_recv) = oneshot::channel();
                            let (confirm_sender, confirm_recv) = oneshot::channel();
                            to_send.push(Install {
                                resource,
                                drop_req: req_recv,
                                confirm_drop: confirm_sender,
                                cfg: format!("{:?}", sub),
                            });
                            new_cache.push((
                                sub,
                                Arc::new(RemoteDrop {
                                    request_drop: Some(req_sender),
                                    drop_confirmed: Some(confirm_recv),
                                }),
                            ));
                        }
                        Err(e) => {
                            let msg = format!("Creationg of {} for {:?} failed: {}", name, sub, e);
                            debug!("{}", msg); // The error will appear together for the validator
                            results.merge(ValidationResult::error(msg));
                        }
                    }
                }
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

        builder.config_validator(validator).tokio_task(installer)
    }
}

fn default_host() -> String {
    "::".to_owned()
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TcpListen {
    port: u16,
    #[serde(default = "default_host")]
    host: String,
}

impl TcpListen {
    pub fn create(&self) -> Result<TcpListener, Error> {
        let std_socket = StdTcpListener::bind((&self.host as &str, self.port))?;
        let tokio_socket = TcpListener::from_std(std_socket, &Handle::default())?;
        Ok(tokio_socket)
    }

    pub fn helper<Extract, ExtractIt, Conn, ConnFut, Name, S, O, C>(
        extract: Extract,
        conn: Conn,
        name: Name,
    ) -> impl Helper<S, O, C>
    where
        S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
        for<'de> C: Deserialize<'de> + Send + Sync + 'static,
        O: Debug + StructOpt + Sync + Send + 'static,
        Extract: FnMut(&C) -> ExtractIt + Send + 'static,
        ExtractIt: IntoIterator<Item = Self>,
        Conn: Fn(&Arc<Spirit<S, O, C>>, TcpStream) -> ConnFut + Sync + Send + 'static,
        ConnFut: Future<Item = (), Error = Error> + Send + 'static,
        Name: Clone + Display + Send + Sync + 'static,
    {
        let conn = Arc::new(conn);
        // TODO: Better logging
        let to_task = move |spirit: &Arc<Spirit<S, O, C>>, listener: TcpListener| {
            let spirit = Arc::clone(spirit);
            let conn = Arc::clone(&conn);
            listener.incoming()
                // FIXME: tk-listen to ignore things like the other side closing connection before we
                // accept
                .map_err(Error::from)
                .for_each(move |new_conn| {
                    let handle_conn = conn(&spirit, new_conn)
                        .or_else(move |e| {
                            // TODO: Better logging, with name
                            warn!("Failed to handle connection: {}", e);
                            future::ok(())
                        });
                    tokio::spawn(handle_conn);
                    future::ok(())
                })
        };
        Task {
            extract,
            build: TcpListen::create,
            to_task,
            name,
        }
    }
}
