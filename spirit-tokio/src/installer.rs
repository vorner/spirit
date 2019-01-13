use failure::Error;
use futures::{Future, IntoFuture, Stream};
use futures::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use futures::sync::oneshot::{self, Receiver, Sender};
use serde::de::DeserializeOwned;
use spirit::fragment::Installer;
use spirit::extension::Extensible;
use structopt::StructOpt;

use runtime::Runtime;

// TODO: Make this publicly creatable
pub struct RemoteDrop {
    name: &'static str,
    request_drop: Option<Sender<()>>,
    drop_confirmed: Option<Receiver<()>>,
}

impl Drop for RemoteDrop {
    fn drop(&mut self) {
        trace!("Requesting remote drop on {}", self.name);
        // Ask the other side to drop the thing
        let _ = self.request_drop.take().unwrap().send(());
        // And wait for it to actually happen
        let _ = self.drop_confirmed.take().unwrap().wait();
        trace!("Remote drop done on {}", self.name);
    }
}

struct Install<R> {
    resource: R,
    drop_req: Receiver<()>,
    confirm_drop: Sender<()>,
}

impl<R> Install<R>
where
    R: IntoFuture<Item = (), Error = ()> + Send,
    R::Future: Send + 'static,
{
    fn spawn(self, name: &'static str) {
        let drop_req = self.drop_req;
        let confirm_drop = self.confirm_drop;
        let fut = self.resource
            .into_future()
            .map_err(move |()| error!("{} unexpectedly failed", name))
            .select(drop_req.map_err(|_| ()))
            .then(move |orig| {
                // Just make sure the original future is dropped first
                drop(orig);
                // Nobody listening for that is fine.
                let _ = confirm_drop.send(());
                debug!("Terminated resource {}", name);
                Ok(())
            });

        tokio::spawn(fut);
    }
}

pub struct FutureInstaller<R> {
    receiver: Option<UnboundedReceiver<Install<R>>>,
    sender: UnboundedSender<Install<R>>,
}

impl<R> Default for FutureInstaller<R> {
    fn default() -> Self {
        let (sender, receiver) = mpsc::unbounded();
        FutureInstaller {
            receiver: Some(receiver),
            sender,
        }
    }
}

impl<R, O, C> Installer<R, O, C> for FutureInstaller<R>
where
    R: IntoFuture<Item = (), Error = ()> + Send + 'static,
    R::Future: Send + 'static,
{
    type UninstallHandle = RemoteDrop;
    fn install(&mut self, resource: R, name: &'static str) -> RemoteDrop {
        let (drop_send, drop_recv) = oneshot::channel();
        let (confirm_send, confirm_recv) = oneshot::channel();
        let sent = self.sender.unbounded_send(Install {
            resource,
            drop_req: drop_recv,
            confirm_drop: confirm_send,
        });
        if sent.is_err() {
            warn!("Remote installer end of {} no longer listens (shutting down?)", name);
        }
        RemoteDrop {
            name,
            request_drop: Some(drop_send),
            drop_confirmed: Some(confirm_recv),
        }
    }
    fn init<B: Extensible<Opts = O, Config = C, Ok = B>>(
        &mut self,
        builder: B,
        name: &'static str,
    ) -> Result<B, Error>
    where
        B::Config: DeserializeOwned + Send + Sync + 'static,
        B::Opts: StructOpt + Send + Sync + 'static,
    {
        let receiver = self.receiver.take().expect("Init called multiple times");
        let installer = receiver.for_each(move |install| {
            install.spawn(name);
            Ok(())
        });
        builder
            .with_singleton(Runtime::default())
            .run_before(|_| {
                tokio::spawn(installer);
                Ok(())
            })
    }
}
