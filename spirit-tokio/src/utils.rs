//! Utility and glue functions

use std::fmt::{Debug, Display};
use std::iter;
use std::sync::Arc;

use failure::Error;
use futures::future::{self, Future, IntoFuture};
use futures::sync::{mpsc, oneshot};
use futures::Stream;
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use spirit::helpers::Helper;
use spirit::validation::{Result as ValidationResult, Results as ValidationResults};
use spirit::{Builder, Spirit};
use structopt::StructOpt;
use tk_listen::ListenExt;

use base_traits::{Name, ResourceConfig, ResourceConsumer};
use net::{IntoIncoming, ListenLimits};
use runtime::Runtime;

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

/// Creates a [`ResourceConsumer`] that takes a listener and runs the provided closure on each
/// accepted connection.
///
/// This accepts two closures. The first one is run once per each resource to initialize a context
/// for that resource.
///
/// Then, whenever a connection is accepted on the resource, the second closure is run (with the
/// connection, the configuration and the context created by the `init`). The returned future is
/// spawned and becomes an independent task (therefore the connections can be handled by multiple
/// threads if they are available). However, the number of parallel connections per resource
/// instance is limited. If the number is exceeded, new connections won't be accepted until some of
/// them terminate.
///
/// Dropping the listener future (the one created by this [`ResourceConsumer`]), for example when
/// reconfiguring, will not drop the already spawned connections. However, the current count of
/// parallel connections won't be transferred and starts at 0.
///
/// If you don't care about some per-listener context, you can prefer the simpler
/// [`per_connection`].
///
/// The resource accepted by this consumer must implement the [`IntoIncoming`] trait (that
/// abstracts over listeners that can accept connections). The [`ResourceConfig`] must also be
/// [`WithListenLimits`].
///
/// [`WithListenLimits`]: ::net::WithListenLimits
pub fn per_connection_init<Config, I, F, R, O, C, Ctx>(
    init: I,
    action: F,
) -> impl ResourceConsumer<Config, O, C>
where
    Config: ListenLimits + ResourceConfig<O, C>,
    Config::Resource: IntoIncoming,
    I: Fn(&Arc<Spirit<O, C>>, &Arc<Config>, &mut Config::Resource, &str) -> Ctx
        + Send
        + Sync
        + 'static,
    F: Fn(
            &Arc<Spirit<O, C>>,
            &Arc<Config>,
            &mut Ctx,
            <Config::Resource as IntoIncoming>::Connection,
            &str,
        ) -> R
        + Sync
        + Send
        + 'static,
    R: IntoFuture<Item = ()>,
    R::Error: Display,
    R::Future: Sync + Send + 'static,
    O: Send + Sync + 'static,
    C: Send + Sync + 'static,
    Ctx: Send + Sync + 'static,
{
    let action = Arc::new(action);
    move |spirit: &_, config: &Arc<Config>, mut listener: Config::Resource, name: &str| {
        let mut ctx = init(spirit, config, &mut listener, &name);
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
                let handle_conn = action(&spirit, &config, &mut ctx, new_conn, &name)
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

/// A simplified version of [`per_connection_init`].
///
/// This simpler version doesn't have the initialization phase and doesn't handle per-listener
/// context. Usually it is enough and should be preferred.
pub fn per_connection<Config, F, R, O, C>(action: F) -> impl ResourceConsumer<Config, O, C>
where
    Config: ListenLimits + ResourceConfig<O, C>,
    Config::Resource: IntoIncoming,
    F: Fn(
            &Arc<Spirit<O, C>>,
            &Arc<Config>,
            <Config::Resource as IntoIncoming>::Connection,
            &str,
        ) -> R
        + Sync
        + Send
        + 'static,
    R: IntoFuture<Item = ()>,
    R::Error: Display,
    R::Future: Sync + Send + 'static,
    O: Send + Sync + 'static,
    C: Send + Sync + 'static,
{
    per_connection_init(
        |_: &_, _: &_, _: &mut _, _: &str| (),
        move |spirit: &_, config: &_, _: &mut (), connection, name: &str| {
            action(spirit, config, connection, name)
        },
    )
}

/// Binds a [`ResourceConfig`] and [`ResourceConsumer`] together to form a [`Helper`].
///
/// This takes an extractor function returning an iterator of [`ResourceConfig`]s from the
/// configuration. Then it binds it together with the provided [`ResourceConsumer`] and takes care
/// of reconfiguring the resources, spawning the appropriate number of them, etc.
///
/// If the configuration always contains exactly one instance, use [`resource`].
///
/// Oftentimes, the configuration fragments also implement [`IteratedCfgHelper`], so it can be used
/// with [`config_helper`] too.
///
/// [`IteratedCfgHelper`]: ::spirit::helpers::IteratedCfgHelper
/// [`config_helper`]: ::spirit::Builder::config_helper
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
    E: FnMut(&C) -> R + Send + 'static,
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

/// Similar to [`resources`], but for just a single instance of the resource configuration.
///
/// Use this instead of [`resources`] if the configuration always contains exactly one of the
/// relevant [`ResourceConfig`].
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
    E: FnMut(&C) -> Config + Send + 'static,
    N: Name,
{
    resources(move |c: &C| iter::once(extract(c)), consumer, name)
}
