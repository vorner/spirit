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

use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::thread::Builder as ThreadBuilder;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use failure::Error;
use fern::Dispatch;
use log::{Level, LevelFilter, Log, Metadata, Record};
use parking_lot::{Condvar, Mutex};
use spirit::extension::{Extensible, Extension};
use spirit::fragment::Transformation;

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
        let mut done = self.done.lock();
        while !*done {
            self.wakeup.wait(&mut done);
        }
    }
}

struct DropNotify(Arc<FlushDone>);

impl Drop for DropNotify {
    fn drop(&mut self) {
        *self.0.done.lock() = true;
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
        thread: Option<String>,
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
                super::LOG_THREAD_NAME.with(|n| n.replace(thread).is_none());
                dst.log(
                    &Record::builder()
                        .args(format_args!("{}", msg))
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
                        self.shared.logger.log(
                            &Record::builder()
                                .args(format_args!("Lost {} messages", lost_msgs))
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
pub enum OverflowMode {
    /// Blocks until there's enough space to push the message.
    Block,

    /// If there's not enough space in the channel, the message is dropped and counted.
    ///
    /// Subsequently, the thread will log how many messages were lost.
    DropMsg,

    /// Drop the messages that don't without any indication it happened.
    DropMsgSilently,
    #[doc(hidden)]
    #[allow(non_camel_case_types)]
    __NON_EXHAUSTIVE__,
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
    pub fn new(logger: Box<dyn Log>, buffer: usize, mode: OverflowMode) -> Self {
        let shared = Arc::new(SyncLogger {
            logger,
            lost_msgs: AtomicUsize::new(0),
        });
        let (sender, receiver) = crossbeam_channel::bounded(buffer);
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
        record.level() <= log::max_level() && self.shared.logger.enabled(metadata)
    }
    fn log(&self, record: &Record) {
        // Don't allocate bunch of strings if the log message would get thrown away anyway.
        // Do the cheap check first to avoid calling through the virtual table & doing arbitrary
        // stuff of the logger.
        if self.enabled(record.metadata()) {
            let i = Instruction::Msg {
                file: record.file().map(ToOwned::to_owned),
                level: record.level(),
                line: record.line(),
                module_path: record.module_path().map(ToOwned::to_owned),
                msg: format!("{}", record.args()),
                target: record.target().to_owned(),
                thread: thread::current().name().map(ToOwned::to_owned),
            };
            if self.mode == OverflowMode::Block {
                self.ch.send(i).expect("Logging thread disappeared");
            } else if let Err(e) = self.ch.try_send(i) {
                assert!(e.is_full(), "Logging thread disappeared");
                if self.mode == OverflowMode::DropMsg {
                    self.shared.lost_msgs.fetch_add(1, Ordering::Relaxed);
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
/// use spirit::prelude::*;
/// use spirit_log::{Background, Cfg as LogCfg, FlushGuard, OverflowMode};
///
/// #[derive(Clone, Debug, Default, Deserialize)]
/// struct Cfg {
///     #[serde(flatten)]
///     log: LogCfg,
/// }
///
/// impl Cfg {
///     fn log(&self) -> LogCfg {
///         self.log.clone()
///     }
/// }
///
/// fn main() {
///     Spirit::<Empty, Cfg>::new()
///         .with(
///             Pipeline::new("logging")
///                 .extract_cfg(Cfg::log)
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
    ) -> Result<(LevelFilter, Box<dyn Log>), Error> {
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
    fn apply(self, builder: E) -> Result<E, Error> {
        let builder = builder.autojoin_bg_thread().keep_guard(self);
        Ok(builder)
    }
}
