/*!
Low and high level APIs

The way most of the documentation describes `spirit`, it is meant as batteries included manager of
your application. While that might speed up development and take care of everything, sometimes you
might need to do things your way, not the way the library wants to. So this section describes the
various levels of the API, so you can take advantage of parts of what is offered while doing the
rest in a custom way.

Let's start with the lowest tier first.

# Loading of configuration

The first tier helps with configuration loading. You specify the structures that should be
filled with the configuration or command line options. `Spirit` reads the command line, finds the
relevant configuration files (depending on both compiled-in values and values on the command
line), scans configuration directories and hands the configuration to you.

You can also ask it to reload the configuration later on, to see if it changed (but you do so
manually).

This basic configuration loading lives in the [`cfg_loader`][crate::cfg_loader] module.

```rust
use serde::Deserialize;
use spirit::{AnyError, ConfigBuilder, Empty};
use spirit::cfg_loader::Builder;

#[derive(Debug, Default, Deserialize)]
struct Cfg {
    message: String,
}

static DEFAULT_CFG: &str = r#"
message = "hello"
"#;

fn main() -> Result<(), AnyError> {
    // Don't care about command line options - there are none in addition to specifying the
    // configuration. If we wanted some more config options, we would use a StructOpt
    // structure instead of Empty.
    //
    // If the user specifies invalid options, a help is printed and the application exits.
    let (Empty {}, mut loader) = Builder::new()
        .config_defaults(DEFAULT_CFG)
        .build();

    // This can be done as many times as needed, to load fresh configuration.
    let cfg: Cfg = loader.load()?;

    // The interesting stuff of your application.
    println!("{}", cfg.message);
    Ok(())
}
```

# Prefabricated fragments of configuration

*Having* your configuration is not enough. You need to *do* something with the configuration.
And if it is something specific to your service, then there's nothing much `spirit` can do. But
usually, there's a lot of the common functionality ‒ you want to configure logging, ports your
service listens on, etc.

For that reason, there are additional crates that each bring some little fragment you can reuse
in your configuration. That fragment provides the configuration options that'll appear inside
the configuration. But it also comes with functionality to create whatever it is being
configured with just a method call.

Most of them are described by the [`Fragment`][crate::fragment::Fragment] trait which allows it
to participate in some further tiers.

Currently, there are these crates with fragments:

* [`spirit-daemonize`]: Configuration and routines to go into background and be a nice daemon.
* [`spirit-dipstick`]: Configuration of the dipstick metrics library.
* [`spirit-log`]: Configuration of logging.
* [`spirit-tokio`]: Integrates basic tokio primitives ‒ auto-reconfiguration for TCP and UDP
  sockets and starting the runtime.
* [`spirit-hyper`]: Integrates the hyper web server.
* [`spirit-reqwest`]: Configuration for the reqwest HTTP [`Client`][reqwest-client].

Also, while this is not outright a configuration fragment, it comes close. When you build your
configuration from the fragments, there's a lot of options. The [`spirit-cfg-helpers`] crate
brings the `--help-config` and `--dump-config` command line options, that describe what options
can be configured and what values would be used after combining all configuration sources
together.

You can create your own fragments and, if it's something others could use, share them.

```rust
use log::info;
use serde::Deserialize;
use spirit::AnyError;
use spirit::cfg_loader::{Builder, ConfigBuilder};
use spirit::fragment::Fragment;
use spirit_log::{Cfg as LogCfg, CfgAndOpts as Logging, Opts as LogOpts};
use structopt::StructOpt;

#[derive(Debug, Default, Deserialize)]
struct Cfg {
    message: String,
    // Some configuration options to configure logging.
    #[serde(default, skip_serializing_if = "LogCfg::is_empty")]
    logging: LogCfg,
}

#[derive(Clone, Debug, StructOpt)]
struct Opts {
    // And some command line switches to also interact with logging.
    #[structopt(flatten)]
    logging: LogOpts,
}

static DEFAULT_CFG: &str = r#"
message = "hello"
"#;

fn main() -> Result<(), AnyError> {
    // Here we added
    let (opts, mut loader): (Opts, _) = Builder::new()
        .config_defaults(DEFAULT_CFG)
        .build();
    let cfg: Cfg = loader.load()?;

    // We put them together (speciality of the logging fragments ‒ some other fragments come
    // only in the configuration).
    let logging = Logging {
        cfg: cfg.logging,
        opts: opts.logging,
    };
    // And here we get ready-made top level logger we can use.
    // (the "logging" string helps to identify fragments in logs ‒ when stuff gets complex,
    // naming things helps).
    //
    // This can configure multiple loggers at once (STDOUT, files, network…).
    let logger = logging.create("logging")?;
    // This apply is from the fern crate. It's one-time initialization. If you want to update
    // logging at runtime, see the next section.
    logger.apply()?;

    // The interesting stuff of your application.
    info!("{}", cfg.message);
    Ok(())
}
```

# Application lifetime management

There's the [`Spirit`] object (and its [`Builder`]) you can use.
It'll start by loading the configuration. It'll also wait for signals (like `SIGHUP` or
`SIGTERM`) and reload configuration as needed, terminate the application, provide access to the
currently loaded configuration, etc. This is done in a background thread which registers the
signals using [`signal_hook`].

You can attach callbacks to it that'll get called at appropriate times ‒ when the configuration
is being loaded (and you can refuse the configuration as invalid) or when the application
should terminate.

The callbacks can be added both to the [`Builder`] and to already started [`Spirit`].

You also can have your main body of the application wrapped in the
[`Spirit::run`][crate::SpiritBuilder::run] method. That way any errors returned are properly
logged and the application terminates with non-zero exit status.

Note that the functionality of these is provided through several traits. It is recommended to
import the [`spirit::prelude::*`][crate::prelude] to get all the relevant traits.

```rust
use std::time::Duration;
use std::thread;

use log::{debug, info};
use serde::Deserialize;
use spirit::Spirit;
use spirit::prelude::*;
use spirit::validation::Action;
use spirit_log::{Cfg as LogCfg, CfgAndOpts as Logging, Opts as LogOpts};
use structopt::StructOpt;

#[derive(Debug, Default, Deserialize)]
struct Cfg {
    message: String,
    sleep: u64,
    #[serde(default, skip_serializing_if = "LogCfg::is_empty")]
    logging: LogCfg,
}

#[derive(Clone, Debug, StructOpt)]
struct Opts {
    // And some command line switches to also interact with logging.
    #[structopt(flatten)]
    logging: LogOpts,
}

static DEFAULT_CFG: &str = r#"
message = "hello"
sleep = 2
"#;

fn main() {
    // Sets up spirit_log ‒ it will register panic handler to log panics. It will also prepare
    // the global logger so the actual logger can be replaced multiple times, using the
    // spirit_log::install
    spirit_log::init();
    Spirit::<Opts, Cfg>::new()
        // Provide default values for the configuration
        .config_defaults(DEFAULT_CFG)
        // If the program is passed a directory, load files with these extensions from there
        .config_exts(&["toml", "ini", "json"])
        .on_terminate(|| debug!("Asked to terminate"))
        .config_validator(|_old_cfg, cfg, opts| {
            let logging = Logging {
                opts: opts.logging.clone(),
                cfg: cfg.logging.clone(),
            };
            // Whenever there's a new configuration, create new logging
            let logger = logging.create("logging")?;
            // But postpone the installation until the whole config has been validated and
            // accepted.
            Ok(Action::new().on_success(|| spirit_log::install(logger)))
        })
        // Run the closure, logging the error nicely if it happens (note: no error happens
        // here)
        .run(|spirit: &_| {
            while !spirit.is_terminated() {
                let cfg = spirit.config(); // Get a new version of config every round
                thread::sleep(Duration::from_secs(cfg.sleep));
                info!("{}", cfg.message);
#               spirit.terminate(); // Just to make sure the doc-test terminates
            }
            Ok(())
        });
}
```

# Extensions and pipelines

The crates with fragments actually allow their functionality to happen almost automatically.
Instead of manually registering a callback when eg. the config is reloaded, each fragment can
be either directly registered into the [`Spirit`] (or [`Builder`]) to do its thing whenever
appropriate, or allows building a [`Pipeline`][crate::Pipeline] that handles loading and
reloading the bit of configuration.

As an example, if the application shall listen on a HTTP endpoint, instead of registering an
[`on_config`][crate::Extensible::on_config] callback and creating the server based on the new
configuration (and shutting down the previous one as needed), you build a pipeline. You provide
a function that extracts the HTTP endpoint configuration from the whole configuration, you
provide a closure that attaches the actual service to the server and register the pipeline. The
pipeline then takes care of creating the server (or servers, if the configuration contains eg.
a `Vec` of them), removing stale ones, rolling back the configuration in case something in it
is broken, etc.

Note that some pipelines and extensions are better registered right away, into the [`Builder`]
(daemonization, logging), you might want to register others only when you are ready for them ‒
you may want to start listening on HTTP only once you've loaded all data. In that case you'd
register it into the [`Spirit`] inside the [`run`][crate::SpiritBuilder::run] method.

```rust
use std::time::Duration;
use std::thread;

use log::{debug, info};
use serde::{Serialize, Deserialize};
use spirit::{Pipeline, Spirit};
use spirit::prelude::*;
use spirit_cfg_helpers::Opts as CfgOpts;
use spirit_daemonize::{Daemon, Opts as DaemonOpts};
use spirit_log::{Cfg as LogCfg, CfgAndOpts as Logging, Opts as LogOpts};
use structdoc::StructDoc;
use structopt::StructOpt;

#[derive(Debug, Default, Deserialize, Serialize, StructDoc)]
struct Cfg {
    /// The message to print every now and then.
    message: String,

    /// How long to wait in between messages, in seconds.
    sleep: u64,

    /// How and where to log.
    #[serde(default, skip_serializing_if = "LogCfg::is_empty")]
    logging: LogCfg,

    /// How to switch into the background.
    #[serde(default)]
    daemon: Daemon,
}

#[derive(Clone, Debug, StructOpt)]
struct Opts {
    #[structopt(flatten)]
    logging: LogOpts,
    #[structopt(flatten)]
    daemon: DaemonOpts,
    #[structopt(flatten)]
    cfg_opts: CfgOpts,
}

static DEFAULT_CFG: &str = r#"
message = "hello"
sleep = 2
"#;

fn main() {
    Spirit::<Opts, Cfg>::new()
        // Provide default values for the configuration
        .config_defaults(DEFAULT_CFG)
        // If the program is passed a directory, load files with these extensions from there
        .config_exts(&["toml", "ini", "json"])
        .on_terminate(|| debug!("Asked to terminate"))
        // All the validation, etc, is done for us behind the scene here.
        // Even the spirit_log::init is not needed, the pipeline handles that.
        .with(Pipeline::new("logging").extract(|opts: &Opts, cfg: &Cfg| Logging {
            cfg: cfg.logging.clone(),
            opts: opts.logging.clone(),
        }))
        // Also add daemonization
        .with(
            Pipeline::new("daemon")
                .extract(|o: &Opts, c: &Cfg| {
                    o.daemon.transform(c.daemon.clone())
                })
        )
        // Let's provide some --config-help and --config-dump options. These get the
        // information from the documentation strings we provided inside the structures. It
        // also uses the `Serialize` trait to provide the dump.
        .with(CfgOpts::extension(|opts: &Opts| &opts.cfg_opts))
        // And some help
        // Run the closure, logging the error nicely if it happens (note: no error happens
        // here)
        .run(|spirit: &_| {
            while !spirit.is_terminated() {
                let cfg = spirit.config(); // Get a new version of config every round
                thread::sleep(Duration::from_secs(cfg.sleep));
                info!("{}", cfg.message);
#               spirit.terminate(); // Just to make sure the doc-test terminates
            }
            Ok(())
        });
}
```

[`Builder`]: crate::Builder
[`Spirit`]: crate::Spirit

*/
