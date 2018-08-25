use std::borrow::Borrow;
use std::collections::HashMap;
use std::fmt::{Debug, Display};
use std::hash::Hash;
use std::marker::PhantomData;
use std::mem;
use std::sync::Arc;

use arc_swap::ArcSwap;
use failure::Error;
use futures::sync::{mpsc, oneshot};
use futures::Future;
use parking_lot::Mutex;
use serde::Deserialize;
use structopt::StructOpt;
use tokio;
use tokio::prelude::*;

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
        // Ask the other side to drop the thing
        let _ = self.request_drop.take().unwrap().send(());
        // And wait for it to actually happen
        let _ = self.drop_confirmed.take().unwrap().wait();
    }
}

pub struct Task<Extract, Build, ToTask, Name> {
    pub extract: Extract,
    pub build: Build,
    pub to_task: ToTask,
    pub name: Name,
}

impl<S, O, C, SubCfg, Resource, Extract, ExtractIt, Build, ToTask, InnerTask, Name> Helper<S, O, C>
    for Task<Extract, Build, ToTask, Name>
where
    S: Borrow<ArcSwap<C>> + Sync + Send + 'static,
    for<'de> C: Deserialize<'de> + Send + Sync + 'static,
    O: Debug + StructOpt + Sync + Send + 'static,
    Extract: FnMut(&C) -> ExtractIt + Send + 'static,
    ExtractIt: IntoIterator<Item = SubCfg>,
    SubCfg: Eq + Debug + Hash + Send + 'static,
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
                // Get the task itself
                let task = to_task(&spirit, resource).into_future();
                // Wrap it in the cancelation routine
                let wrapped = task
                    .map_err(move |e| error!("Task {} on cfg {} failed: {}", name, cfg, e))
                    .select(drop_req.map_err(|_| ())) // Cancelation is OK too
                    .then(|orig| {
                        drop(orig); // Make sure the original future is dropped first.
                        confirm_drop.send(())
                    })
                    .map_err(|_| ()); // If nobody waits for confirm_drop, that's OK.
                tokio::spawn(wrapped)
            })
        };

        let cache = Arc::new(Mutex::new(HashMap::<SubCfg, Arc<RemoteDrop>>::new()));
        let validator = move |_: &Arc<C>, cfg: &mut C, _: &O| -> ValidationResults {
            let mut results = ValidationResults::new();
            let orig_cache = cache.lock();
            let mut new_cache = HashMap::<SubCfg, Arc<RemoteDrop>>::new();
            let mut to_send = Vec::new();
            for sub in extract(cfg) {
                if let Some(previous) = orig_cache.get(&sub).cloned() {
                    debug!("Reusing previous instance of {} for {:?}", name, sub);
                    new_cache.insert(sub, previous);
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
                            new_cache.insert(
                                sub,
                                Arc::new(RemoteDrop {
                                    request_drop: Some(req_sender),
                                    drop_confirmed: Some(confirm_recv),
                                }),
                            );
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
            results.merge(ValidationResult::nothing().on_success(move || {
                for install in to_send {
                    sender
                        .unbounded_send(install)
                        .expect("The tokio end got dropped");
                }
                *cache.lock() = new_cache;
            }));
            results
        };

        builder.config_validator(validator).tokio_task(installer)
    }
}
