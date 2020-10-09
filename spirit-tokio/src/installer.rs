//! Installer of futures.
//!
//! The [`FutureInstaller`] is an [`Installer`] that allows installing (spawning) futures, but also
//! canceling them when they are no longer required by the configuration.
//!
//! [`FutureInstaller`]: crate::installer::FutureInstaller
//! [`Installer`]: spirit::fragment::Installer

use std::future::Future;

use err_context::AnyError;
use log::trace;
use serde::de::DeserializeOwned;
use spirit::extension::Extensible;
use spirit::fragment::Installer;
use structopt::StructOpt;
use tokio::runtime::Handle;
use tokio::select;
use tokio::sync::oneshot::{self, Receiver, Sender};

use crate::runtime::Tokio;

/// An [`UninstallHandle`] for the [`FutureInstaller`].
///
/// This allows to cancel a future when this handle is dropped and wait for it to happen. It is not
/// publicly creatable.
///
/// [`UninstallHandle`]: Installer::UninstallHandle
pub struct RemoteDrop {
    name: &'static str,
    request_drop: Option<Sender<()>>,
    terminated: Option<Receiver<()>>,
}

impl Drop for RemoteDrop {
    fn drop(&mut self) {
        trace!("Requesting remote drop on {}", self.name);
        // Ask the other side to drop the thing
        let _ = self.request_drop.take().unwrap().send(());
        // And wait for it to actually happen
        let mut terminated = self.terminated.take().unwrap();
        if terminated.try_recv().is_err() {
            let _ = Handle::current().block_on(terminated);
        }
        trace!("Remote drop done on {}", self.name);
    }
}

struct SendOnDrop(Option<Sender<()>>);

impl Drop for SendOnDrop {
    fn drop(&mut self) {
        let _ = self.0.take().unwrap().send(());
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
        let (confirm_send, confirm_recv) = oneshot::channel();

        // Make sure we can terminate the future remotely/uninstall it.
        // (Is there a more lightweight version in tokio than bringing in the macros & doing an
        // async block? The futures crate has select function, but we don't have futures as dep and
        // bringing it just for this feels… heavy)
        let cancellable_future = async move {
            // Send the confirmation we're done by RAII. This works with panics and shutdown.
            let _guard = SendOnDrop(Some(confirm_send));

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
            terminated: Some(confirm_recv),
        }
    }

    fn init<E>(&mut self, ext: E, _name: &'static str) -> Result<E, AnyError>
    where
        E: Extensible<Opts = O, Config = C, Ok = E>,
        E::Config: DeserializeOwned + Send + Sync + 'static,
        E::Opts: StructOpt + Send + Sync + 'static,
    {
        ext.with_singleton(Tokio::Default)
    }
}
