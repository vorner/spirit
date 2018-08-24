//! TODO

use std::borrow::Borrow;
use std::fmt::Debug;
use std::mem;
use std::sync::Arc;

use arc_swap::ArcSwap;
use failure::Error;
use futures::Future;
use parking_lot::Mutex;
use serde::Deserialize;
use structopt::StructOpt;
use tokio;
use tokio::prelude::*;

use super::{Builder, Spirit};

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
    /// TODO
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
    /// TODO
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

    /// TODO
    pub fn tokio_run(self) {
        self.run(|spirit| -> Result<(), Error> {
            tokio::run(future::lazy(move || {
                Spirit::tokio_spawn_tasks(&spirit);
                future::ok(())
            }));
            Ok(())
        })
    }
}
