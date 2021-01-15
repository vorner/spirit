/*!
# Proper daemonization and early startup

## What is daemonization

Traditionally, unix services are called daemons. When they start, they go into background instead
of staying in the terminal they were started from. That makes the terminal available for further use
and they don't get killed once the user terminates the session.

While there are alternatives nowdays (`systemd` keeps the services it starts in "foreground", at
least from the service's point of view, there's an external `daemonize` program, ...), it is still
expected "proper" services are able to go into background on their own and have this ability
configurable.

Daemonization, among other things, entails:
* Switching to either the `/` directory or to the daemon's home, to leave the place it was started
  from.
* Possibly switching the user it runs under and dropping privileges.
* Closing `stdin`, `stdout` and `stderr`.
* Detaching from the terminal, by performing `fork` twice.

## The problem

There are some subtleties about the order things are done (some of the problems are general, some
are made even harder by the desire to go into background).

* Printing help (and similar things, like printing configuration) and terminating should happen
  before the program attempts to actually do something. This lower the chance that something is
  undesirable and also the chance it would fail and would not get around to the help.
* Configuring logging, panic handlers and similar should happen as early as possible, to preserve
  the information if the startup fails. Specifically, the program should be logging to a file by
  the time the program tries to go into background.
* Daemonization uses `fork` internally and that one is **not safe** to be performed in a
  multi-threaded program. Therefore, daemonization needs to happen early enough ‒ if any kind of
  initialization starts additional threads, this may be performed only after daemonization is
  complete. Things known to start threads are `tokio` (or other worker threadpools and schedulers)
  and background logging. In particular, it is not possible to perform daemonization in an
  application created with the `#[tokio::main]` annotation and it is not possible to start
  background logging before daemonization.
* Registering signal handlers is recommended before additional threads are started (though failing
  to do that is not inherently UB as with the daemonization, it only contains a tiny race condition
  when chaining multiple signal-handling libraries together).

## The correct order

While multiple solutions are probably possible, here we present one observed to work.

* Install the emergency shutdown signals handling (spirit handles signals and initiates a graceful
  shutdown, but if it gets stuck, we want second termination signal to have an immediate effect).
* Handle the `--help`, `--help-config` and similar early, before any possible side effects or
  fallible operations (unfortunately, the handlers need to parse configuration before
  `--dump-config`, which might come a bit later).
* Configure first-stage logging. This one must happen without background logging (logging in a
  background thread, which might be often more efficient, but we can't afford to do it just yet).
* Perform daemonization.
* Switch to full-featured logging, optionally with the background thread.
* Start all the other things, including ones that may need threads.

Beware that the pipelines and extensions are run in the order of registration (inside each relevant
phase ‒ before-config, config validation, confirmation of thereof, on-config), but these are still
delayed until the start of the `run` method. If you start any threads manually, do so within the
`run`.

## Example

```
use serde::{Deserialize, Serialize};
use spirit::{utils, Pipeline, Spirit};
use spirit::fragment::driver::SilentOnceDriver;
use spirit::prelude::*;
use spirit_cfg_helpers::{Opts as CfgOpts};
use spirit_daemonize::{Daemon, Opts as DaemonOpts};
use spirit_log::{Cfg as Logging, Opts as LogOpts, CfgAndOpts as LogBoth};
use spirit_log::background::{Background, OverflowMode};
use structdoc::StructDoc;
use structopt::StructOpt;

#[derive(Clone, Debug, StructOpt)]
struct Opts {
    #[structopt(flatten)]
    daemon: DaemonOpts,

    #[structopt(flatten)]
    logging: LogOpts,

    #[structopt(flatten)]
    cfg_opts: CfgOpts,

    // Other stuff goes in here
}

impl Opts {
    fn logging(&self) -> LogOpts {
        self.logging.clone()
    }
    fn cfg_opts(&self) -> &CfgOpts {
        &self.cfg_opts
    }
}

#[derive(Clone, Debug, Default, Deserialize, StructDoc, Serialize)]
struct Cfg {
    #[serde(default)]
    daemon: Daemon,

    #[serde(default, skip_serializing_if = "Logging::is_empty")]
    logging: Logging,

    // Other stuff
}

impl Cfg {
    fn logging(&self) -> Logging {
        self.logging.clone()
    }
}

fn main() {
    // Set the emergency signal shutdown
    utils::support_emergency_shutdown().expect("Installing signals isn't supposed to fail");

    Spirit::<Opts, Cfg>::new()
        // Some extra config setup, like `.config_defaults() or `.config_env`

        // Put help options early. They may terminate the program and we may want to do it before
        // daemonization or other side effects.
        .with(CfgOpts::extension(Opts::cfg_opts))
        // Early logging without the background thread. Only once, then it is taken over by the
        // pipeline lower. We can't have the background thread before daemonization.
        .with(
            Pipeline::new("early-logging")
                .extract(|opts: &Opts, cfg: &Cfg| LogBoth {
                    cfg: cfg.logging(),
                    opts: opts.logging(),
                })
                // Make sure this is run only once, at the very beginning.
                .set_driver(SilentOnceDriver::default()),
        )
        // Plug in the daemonization configuration and command line arguments. The library will
        // make it alive ‒ it'll do the actual daemonization based on the config, it only needs to
        // be told it should do so this way.
        //
        // Must come very early, before any threads are started. That includes any potential
        // logging threads.
        .with(unsafe {
            spirit_daemonize::extension(|cfg: &Cfg, opts: &Opts| {
                (cfg.daemon.clone(), opts.daemon.clone())
            })
        })
        // Now we can do the full logging, with a background thread.
        .with(
            Pipeline::new("logging")
                .extract(|opts: &Opts, cfg: &Cfg| LogBoth {
                    cfg: cfg.logging(),
                    opts: opts.logging(),
                })
                .transform(Background::new(100, OverflowMode::Block)),
        )
        // More things and pipelines can go in here, including ones that start threads.
        .run(|_| {
            // And we can start more threads here manually.
            Ok(())
        });
}
```
*/
