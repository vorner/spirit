#![doc(test(attr(deny(warnings))))]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! Configuration support for the [dipstick] metrics library.
//!
//! This provides a configuration [`Fragment`] for the [`spirit`] family of libraries. It
//! configures the „backend“ part of the library ‒ the part that sends the metrics somewhere, like
//! to statsd or a file.
//!
//! # Examples
//!
//! ```rust
//! use dipstick::{stats_all, InputScope};
//! use serde::Deserialize;
//! use spirit::{Empty, Pipeline, Spirit};
//! use spirit::prelude::*;
//! use spirit_dipstick::{Config as MetricsConfig, Monitor};
//!
//! #[derive(Debug, Default, Deserialize)]
//! struct Cfg {
//!     metrics: MetricsConfig,
//! }
//!
//! impl Cfg {
//!    fn metrics(&self) -> &MetricsConfig {
//!         &self.metrics
//!    }
//! }
//!
//! const CFG: &str = r#"
//! [metrics]
//! prefix = "example" # If omitted, the name of the application is used
//! flush-period = "5s"  # Dump metric statistics every 5 seconds
//! backends = [
//!     { type = "file", filename = "/tmp/metrics.txt" },
//!     { type = "stdout" },
//! ]
//! "#;
//!
//! fn main() {
//!    let root = Monitor::new();
//!
//!     Spirit::<Empty, Cfg>::new()
//!        .config_defaults(CFG)
//!        .with(
//!            Pipeline::new("metrics")
//!                .extract_cfg(Cfg::metrics)
//!                .install(root.installer(stats_all)),
//!        )
//!        .run(move |_| {
//!            let counter = root.counter("looped");
//!            counter.count(1);
//!            Ok(())
//!        });
//!}
//! ```

use std::io::Result as IoResult;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dipstick::{
    AtomicBucket, Cancel, CancelHandle, Flush, Graphite, InputKind, InputMetric, InputScope,
    MetricName, MetricValue, MultiInput, NameParts, Observe, ObserveWhen, Prefixed, Prometheus,
    ScheduleFlush, ScoreType, Statsd, Stream,
};
use err_context::prelude::*;
use log::{debug, error};
use serde::{Deserialize, Serialize};
use spirit::fragment::driver::CacheEq;
use spirit::fragment::{Fragment, Installer, Optional};
use spirit::AnyError;

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
    fn add_to(&self, out: &MultiInput) -> Result<MultiInput, AnyError> {
        match self {
            Backend::Stdout => Ok(out.add_target(Stream::write_to_stdout())),
            Backend::Stderr => Ok(out.add_target(Stream::write_to_stderr())),
            Backend::File { filename } => Stream::write_to_file(filename)
                .map(|s| out.add_target(s))
                .with_context(|_| format!("Failed to create metrics file {}", filename.display()))
                .map_err(Box::from),
            Backend::Graphite { host, port } => Graphite::send_to((host as &str, *port))
                .map(|g| out.add_target(g))
                .with_context(|_| format!("Error sending to graphite {host}:{port}"))
                .map_err(Box::from),
            Backend::Prometheus { url } => Prometheus::push_to(url as &str)
                .map(|p| out.add_target(p))
                .with_context(|_| format!("Error sending to prometheus {url}"))
                .map_err(Box::from),
            Backend::Statsd { host, port } => Statsd::send_to((host as &str, *port))
                .map(|s| out.add_target(s))
                .with_context(|_| format!("Error sending to statsd {host}:{port}"))
                .map_err(Box::from),
        }
    }
}

/// An intermediate resource produced by [`Config`].
///
/// This contains all the parts ready to be used.
#[non_exhaustive]
pub struct Backends {
    /// A composed output for the metrics.
    ///
    /// This can be manually installed into an [`AtomicBucket`], or automatically used through a
    /// pipeline and installed into the [`Monitor`].
    pub outputs: MultiInput,

    /// The configured prefix at the root of the metrics tree.
    pub prefix: String,

    /// How often should the metrics be sent.
    pub flush_period: Duration,
}

fn app_name() -> String {
    std::env::args_os()
        .next()
        .and_then(|p| {
            Path::new(&p)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| env!("CARGO_PKG_NAME").to_owned())
}

const fn default_flush() -> Duration {
    Duration::from_secs(60)
}

/// The [`Fragment`] to configure [`dipstick`]s backends.
///
/// This contains the configuration options to configure where the metrics go.
///
/// If you want to be able to turn the metrics off completely, use `Option<Config>`.
///
/// Some of the fields are publicly available and accessible, as they might be useful for other
/// purposes ‒ like generating some metrics at the same frequency as they are being sent.
///
/// # Examples
///
/// ```rust
/// use serde::Deserialize;
/// use spirit_dipstick::Config;
///
/// #[derive(Default, Deserialize)]
/// # #[allow(dead_code)]
/// struct Cfg {
///     metrics: Config,
/// }
/// ```
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "cfg-help", derive(structdoc::StructDoc))]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    /// The prefix ‒ first level of the metrics tree, under which all the metrics are stored.
    ///
    /// If not specifie, it defaults to the application name.
    #[serde(default = "app_name")]
    pub prefix: String,

    /// An interval at which the metrics are sent in aggregated form.
    ///
    /// The metrics are not sent right away, they are buffered and collated for this period of
    /// time, then they are sent in a batch.
    #[serde(
        deserialize_with = "spirit::utils::deserialize_duration",
        serialize_with = "spirit::utils::serialize_duration",
        default = "default_flush"
    )]
    pub flush_period: Duration,

    backends: Vec<Backend>,
}

impl Config {
    fn outputs(&self) -> Result<MultiInput, AnyError> {
        self.backends
            .iter()
            .try_fold(MultiInput::new(), |mo, backend| backend.add_to(&mo))
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

impl Optional for Config {}

impl Fragment for Config {
    type Driver = CacheEq<Config>;
    type Installer = ();
    type Seed = ();
    type Resource = Backends;
    fn make_seed(&self, _name: &str) -> Result<(), AnyError> {
        Ok(())
    }
    fn make_resource(&self, _seed: &mut (), name: &str) -> Result<Backends, AnyError> {
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
        })
    }
}

/// An inner node in a metrics tree.
///
/// This is just a thin wrapper around the [`AtomicBucket`], hiding some of its functionality. That
/// functionality is used for configuration of the backends and how metrics are collated and
/// filtered, which is handled by this library.
///
/// If you want to use the library to just get the configuration and manage it manually, use
/// [`AtomicBucket`] directly. This is for the use with [`Pipeline`]s. See the [crate
/// example](index.html#examples).
///
/// # Cloning
///
/// Cloning is cheap and creates another „handle“ to the same bucket. Therefore, it can be
/// distributed through the application and another layer of [`Arc`] is not needed.
///
/// # Extraction of [`AtomicBucket`]
///
/// The bucket inside can be extracted by the [`Monitor::into_inner`] method, to access
/// functionality hidden by this wrapper. Note that doing so and messing with the bucket might
/// interfere with configuration from this library (the wrapper is more of a road bump than a full
/// barrier).
///
/// # Examples
///
/// ```
/// use dipstick::{InputScope, Prefixed};
/// use spirit_dipstick::Monitor;
///
/// let monitor = Monitor::new();
/// // Plug the monitor.installer() into a pipeline here on startup
/// let sub_monitor = monitor.add_name("sub");
/// let timer = sub_monitor.timer("a-timer");
/// let timer_measurement = timer.start();
/// let counter = sub_monitor.counter("cnt");
/// counter.count(1);
/// timer.stop(timer_measurement);
/// ```
///
/// [`Pipeline`]: spirit::fragment::pipeline::Pipeline
#[derive(Clone, Debug)]
pub struct Monitor(AtomicBucket);

impl Monitor {
    /// Creates a new root node.
    ///
    /// This becomes an independent node. Some backends can be installed into it later on and
    /// metrics or other nodes can be created under it.
    pub fn new() -> Self {
        Self(AtomicBucket::new())
    }

    /// Extracts the internal [`AtomicBucket`]
    ///
    /// See the warning above about doing so ‒ the bucket is actually shared by all clones of the
    /// same [`Monitor`] and therefore you can interfere with the installed backends.
    pub fn into_inner(self) -> AtomicBucket {
        self.0
    }

    /// Creates an installer for installing into this monitor.
    ///
    /// This creates an installer which can be used inside a [`Pipeline`] to attach backends to it.
    ///
    /// The `stats` is the same kind of filtering and selection function the [`dipstick`] uses in
    /// the [`AtomicBucket::stats`].
    ///
    /// [`Pipeline`]: spirit::fragment::pipeline::Pipeline
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
        self.add_name(name)
    }

    fn add_name<S: Into<String>>(&self, name: S) -> Self {
        Monitor(self.0.add_name(name))
    }

    fn named<S: Into<String>>(&self, name: S) -> Self {
        Monitor(self.0.named(name))
    }
}

impl InputScope for Monitor {
    fn new_metric(&self, name: MetricName, kind: InputKind) -> InputMetric {
        self.0.new_metric(name, kind)
    }
}

impl Flush for Monitor {
    fn flush(&self) -> IoResult<()> {
        self.0.flush()
    }
}

impl Observe for Monitor {
    type Inner = <AtomicBucket as Observe>::Inner;
    fn observe<F>(
        &self,
        metric: impl Deref<Target = InputMetric>,
        operation: F,
    ) -> ObserveWhen<Self::Inner, F>
    where
        F: Fn(Instant) -> MetricValue + Send + Sync + 'static,
    {
        self.0.observe(metric, operation)
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

/// An uninstall handle for backends.
///
/// This is used internally and made public only out of necessary. The user doesn't have to
/// interact directly with this.
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
            self.inner.monitor.0.unset_stats();
        }
    }
}

/// The [`Installer`] of backends into the [`Monitor`].
///
/// This is created by [`Monitor::installer`] and used inside of [`Pipeline`]s. The user of this
/// library usually doesn't have to interact with this directly.
///
/// [`Pipeline`]: spirit::fragment::pipeline::Pipeline
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
        self.inner
            .monitor
            .0
            .stats(move |input, name, score| stats(input, name.prepend(&prefix as &str), score));
        self.inner.monitor.0.drain(backends.outputs);
        let cancel_handle = self.inner.monitor.0.flush_every(backends.flush_period);
        Uninstaller {
            inner: Arc::clone(&self.inner),
            orig_gen: cur_gen,
            cancel_handle,
            name,
        }
    }
}
