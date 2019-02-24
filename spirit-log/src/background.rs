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

#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord, Hash)]
pub enum AsyncMode {
    Block,
    DropMsg,
    DropMsgSilent,
}

struct AsyncLogger {
    mode: AsyncMode,
    ch: Sender<Instruction>,
    shared: Arc<SyncLogger>,
}

impl AsyncLogger {
    fn new(logger: Box<dyn Log>, buffer: usize, mode: AsyncMode) -> Self {
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
        self.shared.logger.enabled(metadata)
    }
    fn log(&self, record: &Record) {
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
            if self.mode == AsyncMode::Block {
                self.ch.send(i).expect("Logging thread disappeared");
            } else if let Err(e) = self.ch.try_send(i) {
                assert!(e.is_full(), "Logging thread disappeared");
                if self.mode == AsyncMode::DropMsg {
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

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Background {
    pub mode: AsyncMode,
    pub buffer: usize,
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

pub struct FlushGuard;

impl Drop for FlushGuard {
    fn drop(&mut self) {
        log::logger().flush();
    }
}
