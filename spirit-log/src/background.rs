// The mutex_atomic here is a false positive. We use the mutex because of the condvar.
#![allow(
    unknown_lints,
    renamed_and_removed_lints,
    clippy::unknown_clippy_lints,
    clippy::mutex_atomic
)]
//! Support for logging in the background.
//!
//! The [`AsyncLogger`] can wrap a logger and do the logging in a separate thread. Note that to not
//! lose logs on shutdown, the logger needs to be flushed, either manually or using the
//! [`FlushGuard`].
//!
//! To integrate with the [`Pipeline`], the [`Background`] can be used as a [`Transformation`] of
//! loggers.
//!
//! [`AsyncLogger`]: crate::background::AsyncLogger
//! [`Background`]: crate::background::Background
//! [`FlushGuard`]: crate::background::FlushGuard
//! [`Pipeline`]: spirit::fragment::pipeline::Pipeline
//! [`Transformation`]: spirit::fragment::Transformation

use std::cell::RefCell;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::thread::{Builder as ThreadBuilder, Thread};
use std::time::Duration;

use either::Either;
use fern::Dispatch;
use flume::{Receiver, Sender, TrySendError};
use log::{Level, LevelFilter, Log, Metadata, Record};
use spirit::extension::{Autojoin, Extensible, Extension};
use spirit::fragment::Transformation;
use spirit::AnyError;

thread_local! {
    // The thread name injected by the background logging.
    //
    // There's a chance someone uses sync logging even with the background logging enabled, in
    // which case this'll always stay None (notice that the background logger creates its own
    // thread and only that one sets this, so we can't pollute other thread).
    static LOG_THREAD_NAME: RefCell<Option<Arc<str>>> = RefCell::new(None);

    // Reusable mine thread name, used as a source when putting the log message into the channel.
    //
    // Because we need to potentially log the thread name into multiple loggers, we don't want to
    // clone it every time we take it out from LOG_THREAD_NAME, but we can't have a reference into
    // it both because of refcell and thread-local. We can, however, clone an Arc. When we already
    // have Arcs around, we can as well cache the name in it for the whole lifetime of the thread.
    static MY_THREAD_NAME: Arc<str> = {
        Arc::from(thread::current().name().unwrap_or(super::UNKNOWN_THREAD))
    };
}

fn reset_thread_name() {
    LOG_THREAD_NAME.with(|log| *log.borrow_mut() = None);
}

// In case it we are inside the background logging thread, we have the LOG_THREAD_NAME set (unless
// we log a message ourselves). If not, then we simply take the local thread name.
pub(crate) fn get_thread_name(thread: &Thread) -> Either<&str, Arc<str>> {
    LOG_THREAD_NAME.with(|n| {
        n.borrow()
            .as_ref()
            .map(|n| Either::Right(Arc::clone(n)))
            .unwrap_or_else(|| Either::Left(thread.name().unwrap_or(super::UNKNOWN_THREAD)))
    })
}

struct FlushDone {
    done: Mutex<bool>,
    wakeup: Condvar,
}

impl FlushDone {
    fn new() -> Self {
        Self {
            done: Mutex::new(false),
            wakeup: Condvar::new(),
        }
    }
    fn wait(&self) {
        let mut done = self.done.lock().unwrap();
        while !*done {
            done = self.wakeup.wait(done).unwrap();
        }
    }
}

struct DropNotify(Arc<FlushDone>);

impl Drop for DropNotify {
    fn drop(&mut self) {
        *self.0.done.lock().unwrap() = true;
        self.0.wakeup.notify_all();
    }
}

enum Instruction {
    Msg {
        msg: String,
        level: Level,
        target: String,
        module_path: Option<String>,
        file: Option<String>,
        line: Option<u32>,
        thread: Arc<str>,
    },
    Flush(DropNotify),
}

impl Instruction {
    fn process(self, dst: &dyn Log) {
        match self {
            Instruction::Msg {
                msg,
                level,
                target,
                module_path,
                file,
                line,
                thread,
            } => {
                LOG_THREAD_NAME.with(|n| n.replace(Some(thread)));
                dst.log(
                    &Record::builder()
                        .args(format_args!("{msg}"))
                        .level(level)
                        .target(&target)
                        .file(file.as_ref().map(|f| f as &str))
                        .line(line)
                        .module_path(module_path.as_ref().map(|m| m as &str))
                        .build(),
                );
            }
            Instruction::Flush(done) => {
                dst.flush();
                drop(done);
            }
        }
    }
}

struct SyncLogger {
    logger: Box<dyn Log>,
    lost_msgs: AtomicUsize,
}

struct Recv {
    shared: Arc<SyncLogger>,
    instructions: Receiver<Instruction>,
}

impl Recv {
    fn run(&self) {
        let mut panicked = false;
        loop {
            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                if panicked {
                    reset_thread_name();
                    self.shared.logger.log(
                        &Record::builder()
                            .args(format_args!("Panic in the logger thread, restarted"))
                            .level(Level::Error)
                            .target(module_path!())
                            .line(Some(line!()))
                            .module_path(Some(module_path!()))
                            .build(),
                    );
                }
                for i in &self.instructions {
                    let lost_msgs = self.shared.lost_msgs.swap(0, Ordering::Relaxed);
                    if lost_msgs > 0 {
                        reset_thread_name();
                        self.shared.logger.log(
                            &Record::builder()
                                .args(format_args!("Lost {lost_msgs} messages"))
                                .level(Level::Warn)
                                .target(module_path!())
                                .line(Some(line!()))
                                .module_path(Some(module_path!()))
                                .build(),
                        );
                    }
                    i.process(&*self.shared.logger);
                }
            }));
            if result.is_ok() {
                break;
            }
            panicked = true;
            thread::sleep(Duration::from_millis(100));
        }
        self.shared.logger.flush();
    }
}

/// Selection of how to act if the channel to the logger thread is full.
///
/// This enum is non-exhaustive. Adding more variants in the future will not be considered a
/// breaking change.
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum OverflowMode {
    /// Blocks until there's enough space to push the message.
    Block,

    /// If there's not enough space in the channel, the message is dropped and counted.
    ///
    /// Subsequently, the thread will log how many messages were lost.
    DropMsg,

    /// Drop the messages that don't without any indication it happened.
    DropMsgSilently,

    /// Drop less severe messages sooner than filling the whole buffer.
    ///
    /// If the buffer is completely full, it acts like the [`DropMsg`][OverflowMode::DropMsg]. If
    /// it's not full, but has more than `fill_limit` messages in it, messages with severity
    /// `from_level` or less severe are dropped, while more severe are still inserted into the
    /// buffer.
    ///
    /// Both limits are inclusive.
    AdaptiveDrop {
        /// Level of severity of messages to drop if the buffer is more full that `from_level`.
        from_level: Level,

        /// The level at which the less severe messages start being dropped.
        fill_limit: usize,
    },
}

impl OverflowMode {
    fn count_lost(self) -> bool {
        matches!(
            self,
            OverflowMode::DropMsg | OverflowMode::AdaptiveDrop { .. }
        )
    }
}

/// A logger that postpones the logging into a background thread.
///
/// Note that to not lose messages, the logger need to be flushed.
///
/// Either manually:
///
/// ```rust
/// log::logger().flush();
/// ```
///
/// Or by dropping the [`FlushGuard`] (this is useful to flush even in non-success cases or as
/// integrations with other utilities).
///
/// ```rust
/// # use spirit_log::background::FlushGuard;
/// fn main() {
///     let _guard = FlushGuard;
///     // The rest of the application code.
/// }
/// ```
///
/// Note that even with this, things like [`std::process::exit`] will allow messages to be lost.
pub struct AsyncLogger {
    mode: OverflowMode,
    ch: Sender<Instruction>,
    shared: Arc<SyncLogger>,
}

impl AsyncLogger {
    /// Sends the given logger to a background thread.
    ///
    /// # Params
    ///
    /// * `logger`: The logger to use in the background thread.
    /// * `buffer`: How many messages there can be waiting in the channel to the background thread.
    /// * `mode`: What to do if a message doesn't fit into the channel.
    ///
    /// # Panics
    ///
    /// * If the buffer size is 0.
    /// * If the [`AdaptiveDrop`][OverflowMode::AdaptiveDrop] `fill_limit` is zero or larger or
    ///   equal to `buffer`.
    pub fn new(logger: Box<dyn Log>, buffer: usize, mode: OverflowMode) -> Self {
        assert!(
            buffer > 0,
            "Zero-sized buffer for async logging makes no sense"
        );
        if let OverflowMode::AdaptiveDrop { fill_limit, .. } = mode {
            assert!(fill_limit > 0);
            assert!(fill_limit < buffer);
        }
        let shared = Arc::new(SyncLogger {
            logger,
            lost_msgs: AtomicUsize::new(0),
        });
        let (sender, receiver) = flume::bounded(buffer);
        let recv = Recv {
            shared: Arc::clone(&shared),
            instructions: receiver,
        };
        ThreadBuilder::new()
            .name("spirit-log-bg".to_owned())
            .spawn(move || {
                recv.run();
            })
            .expect("Failed to start logging thread");
        AsyncLogger {
            mode,
            ch: sender,
            shared,
        }
    }
}

impl Log for AsyncLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level() && self.shared.logger.enabled(metadata)
    }
    fn log(&self, record: &Record) {
        // Don't allocate bunch of strings if the log message would get thrown away anyway.
        // Do the cheap check first to avoid calling through the virtual table & doing arbitrary
        // stuff of the logger.
        if self.enabled(record.metadata()) {
            if let OverflowMode::AdaptiveDrop {
                from_level,
                fill_limit,
            } = self.mode
            {
                if record.level() >= from_level && self.ch.len() >= fill_limit {
                    self.shared.lost_msgs.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
            let i = Instruction::Msg {
                file: record.file().map(ToOwned::to_owned),
                level: record.level(),
                line: record.line(),
                module_path: record.module_path().map(ToOwned::to_owned),
                msg: format!("{}", record.args()),
                target: record.target().to_owned(),
                thread: MY_THREAD_NAME.with(|n| Arc::clone(n)),
            };
            if self.mode == OverflowMode::Block {
                self.ch.send(i).expect("Logging thread disappeared");
            } else {
                match self.ch.try_send(i) {
                    Err(TrySendError::Full(_)) if self.mode.count_lost() => {
                        self.shared.lost_msgs.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(TrySendError::Full(_)) | Ok(()) => (),
                    _ => panic!("Logging thread disappeared"),
                }
            }
        }
    }
    fn flush(&self) {
        let done = Arc::new(FlushDone::new());
        self.ch
            .send(Instruction::Flush(DropNotify(Arc::clone(&done))))
            .expect("Logger thread disappeared");
        done.wait();
    }
}

impl Drop for AsyncLogger {
    /// Flushes the logger before going away, to make sure log messages are not lost.
    ///
    /// As much as it's possible to ensure.
    fn drop(&mut self) {
        self.flush();
    }
}

/// A [`Transformation`] to move loggers into background threads.
///
/// By default, loggers created by the [`Pipeline`] are synchronous ‒ they block to do their IO.
/// This puts the IO into a separate thread, with a buffer in between, allowing the rest of the
/// application not to block.
///
/// The same warnings about lost messages and flushing as in the [`AsyncLogger`] case apply here.
/// However, the [`Extensible::keep_guard`] and [`spirit::Extensible::autojoin_bg_thread`] can be
/// used with the [`FlushGuard`] to ensure this happens automatically (the [`FlushGuard`] also
/// implements [`Extension`], which takes care of the setup).
///
/// # Examples
///
/// ```rust
/// use log::info;
/// use serde::Deserialize;
/// use spirit::{Empty, Pipeline, Spirit};
/// use spirit::prelude::*;
/// use spirit_log::{Background, Cfg as LogCfg, FlushGuard, OverflowMode};
///
/// #[derive(Clone, Debug, Default, Deserialize)]
/// struct Cfg {
///     #[serde(default, skip_serializing_if = "LogCfg::is_empty")]
///     logging: LogCfg,
/// }
///
/// impl Cfg {
///     fn logging(&self) -> LogCfg {
///         self.logging.clone()
///     }
/// }
///
/// fn main() {
///     Spirit::<Empty, Cfg>::new()
///         .with(
///             Pipeline::new("logging")
///                 .extract_cfg(Cfg::logging)
///                 .transform(Background::new(100, OverflowMode::Block)),
///         )
///         .with_singleton(FlushGuard)
///         .run(|_spirit| {
/// #           let spirit = std::sync::Arc::clone(_spirit);
/// #           std::thread::spawn(move || spirit.terminate());
///             info!("Hello world");
///             Ok(())
///         });
/// }
/// ```
///
/// [`Pipeline`]: spirit::fragment::pipeline::Pipeline
/// [`Extensible::keep_guard`]: spirit::Extensible::keep_guard
/// [`Extensible::autojoin_bg_thread`]: spirit::Extensible::autojoin_bg_thread
/// [`Extension`]: spirit::extension::Extension
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Background {
    mode: OverflowMode,
    buffer: usize,
}

impl Background {
    /// Creates a new [`Background`] object.
    ///
    /// # Params
    ///
    /// * `buffer`: How many messages fit into the channel to the background thread.
    /// * `mode`: What to do if the current message does not fit.
    pub fn new(buffer: usize, mode: OverflowMode) -> Self {
        Background { mode, buffer }
    }
}

impl<I, F> Transformation<Dispatch, I, F> for Background {
    type OutputResource = (LevelFilter, Box<dyn Log>);
    type OutputInstaller = I;
    fn installer(&mut self, original: I, _name: &'static str) -> I {
        original
    }
    fn transform(
        &mut self,
        dispatch: Dispatch,
        _fragment: &F,
        _name: &'static str,
    ) -> Result<(LevelFilter, Box<dyn Log>), AnyError> {
        let (level, sync_logger) = dispatch.into_log();
        let bg = AsyncLogger::new(sync_logger, self.buffer, self.mode);
        Ok((level, Box::new(bg)))
    }
}

/// This, when dropped, flushes the logger.
///
/// Unless the logger is flushed, there's a risk of losing messages on application termination.
///
/// It can be used either separately or plugged into the spirit [`Builder`] (through the
/// [`Extension`] trait). In that case, it also turns on the [`autojoin_bg_thread`] option, so the
/// application actually waits for the spirit thread to terminate and drops the guard.
///
/// Note that it's fine to flush the logs multiple times (it only costs some performance, because
/// the flush needs to wait for all the queued messages to be written).
///
/// # Examples
///
/// ```rust
/// # use log::info;
/// # use spirit::{Empty, Spirit};
/// # use spirit::prelude::*;
/// # use spirit_log::FlushGuard;
/// Spirit::<Empty, Empty>::new()
///     .with_singleton(FlushGuard)
///     .run(|_spirit| {
/// #       let spirit = std::sync::Arc::clone(_spirit);
/// #       std::thread::spawn(move || spirit.terminate());
///         info!("Hello world");
///         Ok(())
///     });
/// ```
///
/// [`Builder`]: spirit::Builder
/// [`autojoin_bg_thread`]: spirit::Extensible::autojoin_bg_thread
pub struct FlushGuard;

impl FlushGuard {
    /// Performs the flush of the global logger.
    ///
    /// This can be used directly, instead of getting an instance of the [`FlushGuard`] and
    /// dropping it. But both ways have the same effect.
    pub fn flush() {
        log::logger().flush();
    }
}

impl Drop for FlushGuard {
    fn drop(&mut self) {
        Self::flush();
    }
}

impl<E> Extension<E> for FlushGuard
where
    E: Extensible<Ok = E>,
{
    fn apply(self, builder: E) -> Result<E, AnyError> {
        let builder = builder
            .autojoin_bg_thread(Autojoin::TerminateAndJoin)
            .keep_guard(self);
        Ok(builder)
    }
}
