#![allow(clippy::mutex_atomic)] // Mutex<bool> needed for condvar
//! Installer of futures.
//!
//! The [`FutureInstaller`] is an [`Installer`] that allows installing (spawning) futures, but also
//! canceling them when they are no longer required by the configuration.
//!
//! [`FutureInstaller`]: crate::installer::FutureInstaller
//! [`Installer`]: spirit::fragment::Installer

use std::future::Future;
use std::sync::{Arc, Condvar, Mutex};

use err_context::AnyError;
use log::trace;
use serde::de::DeserializeOwned;
use spirit::extension::Extensible;
use spirit::fragment::Installer;
use structopt::StructOpt;
use tokio::select;
use tokio::sync::oneshot::{self, Sender};

use crate::runtime::{self, ShutGuard, Tokio};

/// Wakeup async -> sync.
#[derive(Default, Debug)]
struct Wakeup {
    wakeup: Mutex<bool>,
    condvar: Condvar,
}

impl Wakeup {
    fn wait(&self) {
        trace!("Waiting on wakeup on {:p}/{:?}", self, self);
        let g = self.wakeup.lock().unwrap();
        let _g = self.condvar.wait_while(g, |w| !*w).unwrap();
    }

    fn wakeup(&self) {
        trace!("Waking up {:p}/{:?}", self, self);
        // Expected to be unlocked all the time.
        *self.wakeup.lock().unwrap() = true;
        self.condvar.notify_all();
    }
}

/// An [`UninstallHandle`] for the [`FutureInstaller`].
///
/// This allows to cancel a future when this handle is dropped and wait for it to happen. It is not
/// publicly creatable.
///
/// [`UninstallHandle`]: Installer::UninstallHandle
pub struct RemoteDrop {
    name: &'static str,
    request_drop: Option<Sender<()>>,
    wakeup: Arc<Wakeup>,
    // Prevent the tokio runtime from shutting down too soon, as long as the resource is still
    // alive. We want to remove it first gracefully.
    _shut_guard: Option<ShutGuard>,
}

impl Drop for RemoteDrop {
    fn drop(&mut self) {
        trace!("Requesting remote drop on {}", self.name);
        // Ask the other side to drop the thing
        let _ = self.request_drop.take().unwrap().send(());
        // And wait for it to actually happen
        self.wakeup.wait();
        trace!("Remote drop done on {}", self.name);
    }
}

struct SendOnDrop(Arc<Wakeup>);

impl Drop for SendOnDrop {
    fn drop(&mut self) {
        self.0.wakeup();
    }
}

/// An installer able to install (an uninstall) long-running futures.
///
/// This is an installer that can be used with [`Fragment`] that produce futures.
///
/// * If the spirit does not contain a [`Tokio`] runtime yet, one (the default one) will be added
///   as a singleton. Note that this has effect on how spirit manages lifetime of the application.
/// * An uninstallation is performed by dropping canceling the future.
/// * This works with both concrete types (implementing `Future<Output = ()> + Send`) and boxed
///   ones. Note that the boxed ones are `Pin<Box<dyn Future<Output = ()> + Send>>`, created by
///   [`Box::pin`].
///
/// See the crate level examples for details how to use (the installer is used only as the
/// associated type in the fragment implementation).
///
/// [`Fragment`]: spirit::fragment::Fragment
#[derive(Copy, Clone, Debug, Default)]
pub struct FutureInstaller;

impl<F, O, C> Installer<F, O, C> for FutureInstaller
where
    F: Future<Output = ()> + Send + 'static,
{
    type UninstallHandle = RemoteDrop;

    fn install(&mut self, fut: F, name: &'static str) -> RemoteDrop {
        let (request_send, request_recv) = oneshot::channel();
        let wakeup = Default::default();

        // Make sure we can terminate the future remotely/uninstall it.
        // (Is there a more lightweight version in tokio than bringing in the macros & doing an
        // async block? The futures crate has select function, but we don't have futures as dep and
        // bringing it just for this feels… heavy)
        let guard = SendOnDrop(Arc::clone(&wakeup));
        let cancellable_future = async move {
            // Send the confirmation we're done by RAII. This works with panics and shutdown.
            let _guard = guard;

            select! {
                _ = request_recv => trace!("Future {} requested to terminate", name),
                _ = fut => trace!("Future {} terminated on its own", name),
            };
        };

        trace!("Installing future {}", name);
        tokio::spawn(cancellable_future);

        RemoteDrop {
            name,
            request_drop: Some(request_send),
            wakeup,
            _shut_guard: runtime::shut_guard(),
        }
    }

    fn init<E>(&mut self, ext: E, _name: &'static str) -> Result<E, AnyError>
    where
        E: Extensible<Opts = O, Config = C, Ok = E>,
        E::Config: DeserializeOwned + Send + Sync + 'static,
        E::Opts: StructOpt + Send + Sync + 'static,
    {
        #[cfg(feature = "multithreaded")]
        {
            ext.with_singleton(Tokio::Default)
        }
        #[cfg(not(feature = "multithreaded"))]
        {
            ext.with_singleton(Tokio::SingleThreaded)
        }
    }
}
