#![doc(
    html_root_url = "https://docs.rs/spirit-dipstick/0.1.0/spirit_dipstick/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dipstick::{
    AtomicBucket, CancelHandle, Flush, Graphite, InputKind, InputMetric, InputScope, MetricName,
    MetricValue, MultiOutput, NameParts, Prefixed, Prometheus, Result as DipResult, ScheduleFlush,
    ScoreType, Statsd, Stream,
};
use failure::{Error, ResultExt};
use log::{debug, error};
use serde::{Deserialize, Serialize};
use spirit::fragment::driver::CacheEq;
use spirit::fragment::{Fragment, Installer};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(structdoc::StructDoc))]
#[serde(tag = "type", rename_all = "kebab-case")]
enum Backend {
    Stdout,
    Stderr,
    // Log, TODO: ????
    File { filename: PathBuf },
    Graphite { host: String, port: u16 },
    Prometheus { url: String },
    Statsd { host: String, port: u16 },
}

impl Backend {
    fn add_to(&self, out: &MultiOutput) -> Result<MultiOutput, Error> {
        match self {
            Backend::Stdout => Ok(out.add_target(Stream::to_stdout())),
            Backend::Stderr => Ok(out.add_target(Stream::to_stderr())),
            Backend::File { filename } => {
                // Workaroud until https://github.com/fralalonde/dipstick/pull/53 lands
                let f = File::create(&filename).with_context(|_| {
                    format!("Failed to create metrics file {}", filename.display())
                })?;
                Ok(out.add_target(Stream::write_to(f)))
            }
            Backend::Graphite { host, port } => {
                Graphite::send_to((host as &str, *port)).map(|g| out.add_target(g))
            }
            Backend::Prometheus { url } => {
                Prometheus::push_to(url as &str).map(|p| out.add_target(p))
            }
            Backend::Statsd { host, port } => {
                Statsd::send_to((host as &str, *port)).map(|s| out.add_target(s))
            }
        }
        .map_err(|e| failure::err_msg(e.to_string()))
    }
}

pub struct Backends {
    pub outputs: MultiOutput,
    pub prefix: String,
    pub flush_period: Duration,
    _sentinel: (),
}

fn app_name() -> String {
    env!("CARGO_PKG_NAME").to_owned()
}

const fn default_flush() -> Duration {
    Duration::from_secs(60)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(structdoc::StructDoc))]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    #[serde(default = "app_name")]
    pub prefix: String,

    #[serde(
        deserialize_with = "serde_humantime::deserialize",
        serialize_with = "spirit::utils::serialize_duration",
        default = "default_flush"
    )]
    pub flush_period: Duration,

    backends: Vec<Backend>,
}

impl Config {
    fn outputs(&self) -> Result<MultiOutput, Error> {
        self.backends
            .iter()
            .try_fold(MultiOutput::output(), |mo, backend| backend.add_to(&mo))
    }
}

// Some kind of deserialize-default would be nice
impl Default for Config {
    fn default() -> Self {
        Self {
            prefix: app_name(),
            flush_period: default_flush(),
            backends: Vec::new(),
        }
    }
}

impl Fragment for Config {
    type Driver = CacheEq<Config>;
    type Installer = ();
    type Seed = ();
    type Resource = Backends;
    fn make_seed(&self, _name: &str) -> Result<(), Error> {
        Ok(())
    }
    fn make_resource(&self, _seed: &mut (), name: &str) -> Result<Backends, Error> {
        let outputs = self.outputs()?;
        debug!(
            "Prepared {} metrics outputs for {}",
            self.backends.len(),
            name
        );
        Ok(Backends {
            prefix: self.prefix.to_owned(),
            flush_period: self.flush_period,
            outputs,
            _sentinel: (),
        })
    }
}

#[derive(Clone, Debug)]
pub struct Monitor(AtomicBucket);

impl Monitor {
    pub fn new() -> Self {
        Self(AtomicBucket::new())
    }

    pub fn into_inner(self) -> AtomicBucket {
        self.0
    }

    pub fn installer<F>(&self, stats: F) -> MonitorInstaller<F>
    where
        F: Fn(InputKind, MetricName, ScoreType) -> Option<(InputKind, MetricName, MetricValue)>
            + Send
            + Sync
            + 'static,
    {
        MonitorInstaller::new(self.clone(), stats)
    }
}

impl Default for Monitor {
    fn default() -> Self {
        Self::new()
    }
}

impl Prefixed for Monitor {
    fn get_prefixes(&self) -> &NameParts {
        self.0.get_prefixes()
    }

    fn add_prefix<S: Into<String>>(&self, name: S) -> Self {
        Monitor(self.0.add_prefix(name))
    }
}

impl InputScope for Monitor {
    fn new_metric(&self, name: MetricName, kind: InputKind) -> InputMetric {
        self.0.new_metric(name, kind)
    }
}

impl Flush for Monitor {
    fn flush(&self) -> DipResult<()> {
        self.0.flush()
    }
}

struct InstallerInner {
    monitor: Monitor,
    // The gen is incremented on each new installation. Then, if an uninstallation request comes,
    // we check if it is for the *same* thing ‒ because new installation can replace it and then
    // uninstallation may come later on. We don't want to uninstall the new thing.
    //
    // In practice, everything will be run from the callbacks of spirit. That is protected by mutex
    // already, so we could get away with something like Cell ‒ but Rust doesn't like that, as that
    // is not Send. As this is not performance critical, we are fine with using AtomicUsize
    // instead. Just note that there won't be any races about what is stored in there.
    gen: AtomicUsize,
}

impl InstallerInner {
    fn try_flush(&self, name: &str) {
        if let Err(e) = self.monitor.flush() {
            error!("Failed to flush {}: {}", name, e);
        }
    }
}

pub struct Uninstaller {
    inner: Arc<InstallerInner>,
    orig_gen: usize,
    cancel_handle: CancelHandle,
    name: &'static str,
}

impl Drop for Uninstaller {
    fn drop(&mut self) {
        self.cancel_handle.cancel();
        if self.orig_gen == self.inner.gen.load(Ordering::Relaxed) {
            debug!(
                "Uninstalling backends gen {} from {}",
                self.orig_gen, self.name
            );
            self.inner.try_flush(self.name);
            // Remove the backends only if it's not replaced by a newer one. The newer one would
            // have its own Uninstaller
            self.inner.monitor.0.unset_drain();
            // TODO: Once this is no longer generic…
            // self.inner.monitor.0.unset_stats();
        }
    }
}

pub struct MonitorInstaller<F> {
    inner: Arc<InstallerInner>,
    stats: Arc<F>,
}

impl<F> MonitorInstaller<F> {
    fn new(monitor: Monitor, stats: F) -> Self {
        Self {
            inner: Arc::new(InstallerInner {
                monitor,
                gen: AtomicUsize::new(0),
            }),
            stats: Arc::new(stats),
        }
    }
}

impl<F, O, C> Installer<Backends, O, C> for MonitorInstaller<F>
where
    F: Fn(InputKind, MetricName, ScoreType) -> Option<(InputKind, MetricName, MetricValue)>
        + Send
        + Sync
        + 'static,
{
    type UninstallHandle = Uninstaller;
    fn install(&mut self, backends: Backends, name: &'static str) -> Uninstaller {
        debug!(
            "Setting metrics backends for {} with prefix {}",
            name, backends.prefix
        );
        self.inner.try_flush(name);
        // Note: fetch_add returns the previous value, that's why we add to both
        let cur_gen = self
            .inner
            .gen
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let stats = Arc::clone(&self.stats);
        let prefix = backends.prefix;
        self.inner.monitor.0.set_stats(move |input, name, score| {
            stats(input, name.prepend(&prefix as &str), score)
        });
        self.inner.monitor.0.set_drain(backends.outputs);
        let cancel_handle = self.inner.monitor.0.flush_every(backends.flush_period);
        Uninstaller {
            inner: Arc::clone(&self.inner),
            orig_gen: cur_gen,
            cancel_handle,
            name,
        }
    }
}
