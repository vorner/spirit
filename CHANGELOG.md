# spirit-0.4.21
# spirit-tokio-0.9.2
# spirit-daemonize-0.5.1

* Version updates. Fixes dep security advisories under spirit-daemonize (#69,
  #70).

# spirit-0.4.20

* Simplifying the testing example.

# spirit-0.4.19

* `App::run_test` for easier management of spirit mock "applications" inside
  tests.
* Added guide for the above testing.

# spirit-0.4.18

* Fix panic in `on_terminate` called after terminating.

# spirit-log-0.4.4

* The Opts are now Default.

# spirit-0.4.17

* Ability to inject command line options instead of parsing real ones (primarily
  for testing).

# spirit-reqwest-0.5.1

* Implement support for `tcp_keepalive`.

# spirit-tokio-0.9.1

* Implement `From` instead of `Into` around `Either`.

# spirit-hyper-0.9.0

* Dep updates.

# spirit-tokio-0.9.0

* Fix links in docs.
* Require less Unpin bounds (if the bound is not satisfied, the outer type is
  neither, but it's better than being 100% unusable).
* The Accept trait now takes Pin.

# spirit-0.4.16

* More flushing of logs, in more corner cases.

# spirit-0.4.15

* Flush logs before terminating.

# spirit-log-0.4.3

* Fix invalid panic when using the background thread, non-blocking overflow mode
  is used and overflow _doesn't_ happen.
* Install the background flush guard automatically.

# spirit-0.4.14

* Update and extend the guide about daemonization.

# spirit-daemonize-0.5.0

* Use extension.
* Daemonization is unsafe, because of fork.

# spirit-log-0.4.2

* Dep version updates.

# spirit-hyper-0.8.1

* Support/fix graceful shutdown.

# spirit-tokio-0.8.1

* Fix deadlock in the installer unistall routine (the remote drop had reversed
  condition).
* Support/fix graceful shutdown of futures when terminating the runtime.
* Add support for user to hold the runtime alive.
* Add support for removing unix-domain sockets (before and after use,
  configurable; by default stays the same as not deleting them).
* Add support for abstract addresses for unix domain sockets.

# spirit-0.4.13

* Use `utils::support_emergency_shutdown` instead of `cleanup_signals` (the
  latter is deprecated).

# spirit-0.4.12

* Fix occasional deadlock on shutdown of single-threaded tokio based
  applications.

# spirit-reqwest-0.5.0

* Porting to reqwest 0.11.

# spirit-hyper-0.8.0

* Porting to hyper 0.14.

# spirit-tokio-0.8.0

* Porting to tokio 1.0.

# 0.4.11

* Guide.
* More info about received signals.

# 0.4.10

* Migrate to signal-hook-0.2 (internal change)

# spirit-cfg-helpers-0.4.0

* Use arc-swap 1.0

# spirit-0.4.9

* `Pipeline::and_then` to conveniently support fallible transformations.
* Use arc-swap 1.0
* Some doc fixes

# spirit-reqwest-0.4.2

* Ability to disable proxy (including auto-detected one).

# spirit-0.4.8

* `is_default` and `is_true` utility functions for `skip_serializing_if`.

# spirit-0.4.7

* `spirit::utils::cleanup_signals` to support "Second CTRL?+C terminates
  immediately".

# spirit-hyper-0.7.1

* Make the `h1_writev` optional, as there's an auto mode by default. We want to
  have that, most of the time.
  - Technically a breaking change, but very small one and at a place where
    people are not really expected to touch. + We are still a 0. crate, so it
    makes more sense this way.
* Hide some more default options from `--dump-config`.

# spirit-tokio-0.7.1

* Pass the `poll_read_buf`, `poll_write_buf` through, to keep performance.

# spirit-reqwest-0.4.1

* Workaround for bug in `reqwest` where `blocking::ClientBuilder::from` doesn't
  preserve original timeout set in the async builder.

# spirit-log-0.4.1
# spirit-cfg-helpers-0.3.1

* Remove doc comments in build from `StructOpt` things, to work around problem
  in `structopt`.

# spirit-daemonize-0.4.0

* Move to pipelines.
* Postpone daemonization for after stuff got validated, so errors are shown.
* Remove doc comments in build from `StructOpt` things, to work around problem
  in `structopt`.

# spirit-hyper-0.7.0

* Upgrade to spirit-hyper to 0.13.

# spirit-tokio-0.7.0

* Upgrade to tokio 0.2. Hopefully not much functionality lost on the way. API
  changed, unfortunately.
* Use `socket2` instead of `net2`. Besides `net2` being deprecated, this'll
  allow us some more options for the sockets in the future.

# spirit-0.4.6

* Fix `--version` (taken from the application, not from spirit).
* Don't use hidden/private API of structopt.
* Clearer error message if config loading fails and there was no config passed
  in at all.
* Remove links to the guide. There's nothing in it right now anyway :-(.
* `Extensible::around_hooks` to support wrapping hooks/callbacks/pipelines
  inside a context similar to `Extensible::run_around`.

# spirit-reqwest-0.4.0

* Update to reqwest 0.10.

# spirit-0.4.5
# spirit-daemonize-0.3.2

* Dep updates.

# spirit-log-0.4.0

* Releasing the previous as a breaking version; exposed different version of
  fern as part of public API.

# spirit-log-0.3.1

* `to-syslog` feature renamed to just `syslog`
* Dependency updates

# spirit-daemonize-0.3.1
# spirit-dipstick-0.3.0
# spirit-reqwest-0.3.1
# spirit-tokio-0.6.1
# 0.4.4

* Dependency updates.
  - Get rid of serde-humantime (outdated, unmaintained).
  - Structopt ‒ allow more versions.
  - Dipstick on 0.9
  - Many others
  - Tokio/futures related deps are still outdated :-(
* Use proper `#[exhaustive]` where appropriate instead of hidden workarounds.

# 0.4.3

* Note in readme about looking for contributors.

# 0.4.2

* Fix build against newer versions of structopt.

# 0.4.1

Root:
* Start the work on the guide level documentation.
* Go back to depending on vanilla `config` instead of private fork.

# 0.4.0
# + Bump of everything else

Log:
* Make the syslog support non-default feature.
* Flatten inside, not outside ‒ the caller needs `#[serde(default,
  skip_serializing_if)]` instead of `#[serde(flatten)]`. But the caller can
  choose the name.

Root:
* Get rid of the `Body` hack, `Box<dyn FnOnce()>` now works.
* Abandoning failure, using boxed standard errors (with chaining).
* Removing dependencies and making a lot of things optional, to cut down on the
  dependency graph size.
* Pruning of the prelude ‒ it now contains only traits, not types.
* Ability to ask for all supported extensions when visiting a directory.
* The config loader can now parse arbitrary iterator as command line too.
* StructOpt updated to 0.3 (you'll have to update too).
* Remove ini and hjson from default-feautures
  - Nobody really uses hjson and it has ancient dependencies.
  - Ini is not very expressive.
* Bugfix: Allow overriding non-string config values from command line.
* Warn on unused config options.
  - Can be turned off.
* The background thread is auto-joined by default now.
  - And the terminate is called at the end of the `run` method (after all
    around-bodies).
* Hide the arc-swap from spirit's public interface.
  - Talk by Arcs directly
  - The `cfg_store` goes to `spirit_cfg_helpers` crate (bumping that one is much
    less fuzz).

# 0.3.8

* Make the OnceDriver public.

# spirit-dipstick-0.1.5

* Delegate the Observe trait through the Monitor.

# spirit-dipstick-0.1.4

* Deal with dipstick upstream API breakage.

# spirit-dipstick-0.1.3

* Little internal optimisations.

# spirit-daemonize-0.2.1

* Fix some links in docs.

# 0.3.7

* Docs improvement (mention spirit-dipstick, fix links).

# spirit-dipstick-0.1.2

* Compatibility with dipstick 0.7.2.

# spirit-reqwest-0.2.2

* Addition of several more configuration fields.

# spirit-dipstick-0.1.1

* Fix application name as the default for prefix.
* Metrics to file are appended, not overwritten.

# spirit-tokio-0.5.2

* Ability to configure the threadpool runtime from config file.

# spirit-0.3.6

* A routine to serialize/deserialize `Option<Duration>` in human readable form.

# spirit-dipstick-0.1.0

* First shot at the crate.

# spirit-reqwest-0.2.1

* Internal code cleanups.

# spirit-log-0.2.4

* Fix thread name in case of background logging into multiple loggers.

# spirit-0.3.5

* Add a routine to serialize duration in human-readable format (when there are
  configs with durations). Might as well eventually go to serde-humantime.
* Fix of the CacheEq driver's abort.
* Separate `Optional` trait from `Stackable`, separating ability to use
  `Option<F>` from other collections.

# spirit-0.3.4

* Fix running termination hooks.
* Fix some links in config.

# spirit-log-0.2.3

* Tweaking of AsyncLogger::enabled to do a cheap early check.
* Async logging has adaptive dropping ‒ can be configured to drop less severe
  messages sooner.

# spirit-tokio-0.5.1
# spirit-hyper-0.5.2

* Update to deprecations from spirit 0.3.2

# 0.3.2

* `on_terminate` takes `FnOnce` instead of `FnMut`.
* `log_error!` macro, to cut down on boilerplate of the same-named function.
* `ErrorLogFormat::Multiline` replaced by `MultiLine` (inconsistency, the old
  one still stays as a deprecated alias).
* Fix corner cases around registering callbacks after termination.
* Don't register signals if the background thread is not started.
* `ConfigBuilder::config_defaults_typed` to specify the lowest level of
  configuration through a struct instead of just string.

# spirit-hyper-0.5.1

* Support the http1-half-close option.

# spirit-log-0.2.2

* Cap configured level by `log::STATIC_MAX_LEVEL` too.

# spirit-0.3.2

* bugfix: SeqDriver removes unused resources.

# spirit-log-0.2.1

* Asynchronous/background logging support.
* Fix of collision on stderr (errors were logged twice if configuration had a
  stderr log).

# 0.3.1

* Ability to hold guards (things keeping something alive) until the end of the
  lifetime of Spirit (and therefore application, in most cases).
* Ability to join the background thread (either manually or requesting
  autojoin).
* Fixes around logging of validation errors.
* Fixes around updating sequences in pipelines (only the first update
  succeeded).

# 0.3.0
# spirit-cfg-helpers-0.2.0
# spirit-daemonize-0.2.0
# spirit-hyper-0.5.0
# spirit-log-0.2
# spirit-reqwest-0.2.0
# spirit-tokio-0.5.0

* Configuration can be loaded without the full machinery of full Spirit object.
* Helpers got renamed to Extension.
* Extension, callbacks and other similar things can now be added to already
  built Spirit as well as Builder.
* A lot of methods moved onto traits to support the above. To import all of
  them, `use spirit::prelude::*` is recommended.
* Pieces of configuration are now described in generic way with the `Fragment`
  trait. This allows to manually create the resource the configuration
  describes.
* `CfgHelper` and `IteratedCfgHelper` are gone. They are replaced with the
  `Pipeline` machinery that takes a `Fragment` on one end, creates the resource,
  does something with it and then installs it fully automatically. This is,
  however, more flexible and looks more magical when being read.

# spirit-cfg-helpers-0.1.1

* Fix panic when the config can't be serialized to toml due to order of values.

# 0.2.10
# spirit-daemonize-0.1.3
# spirit-log-0.1.6
# spirit-hyper-0.4.1
# spirit-tokio-0.4.2

* Configuration fragments now implement `Serialize` and `StructDoc` to support
  the `CfgDump` and `CfgHelp` helpers.

# spirit-cfg-helpers-0.1.0

* Initial implementation.
  - `CfgDump` and `CfgHelp` command line options.
  - `config_logging` to log configuration changes.

# spirit-reqwest-0.1.0

* Initial implementation.
  - Fragment to configure and build the client.
  - A storage that is kept up to date all the time.

# 0.2.9

* spirit::utils::Hidden to hide sensitive information from logs.

# spirit-tokio-0.4.1

* Make the `max-conn` connection limit optional. If not present, the number of
  active connections is not limited (well, but max number of file descriptors,
  kernel, memory… but not the application itself).

# spirit-hyper-0.4.0

* Ability to specify connection limits.

# spirit-tokio-0.4.0

* New way of handling connection limits (reusable as a transport for eg. hyper).
* More informative error messages when binding.

# spirit-log-0.1.5

* (Re)enabled log-panics integration. It got lost when splitting of the main
  `spirit`, but was always intended to be present.

# spirit-0.2.8

* Docs extensions (example, listing features, adding link to tutorial).

# spirit-tokio-0.3.1

* Docs fixes.

# spirit-hyper-0.3.0

* Redesigned (same changes as with spirit-tokio).
* Support for arbitrary `Service` implementations.

# spirit-tokio-0.3.0

* Redesigned; the main traits are now `ResourceConfig` and `ResourceConsumer`.
  They interlock in a similar way the serde traits do.
* Support for unix domain sockets.
* Either for alternative sockets (usually unix vs. IP).
* Fixed problem of not shutting down on error from inner body.

# 0.2.7
# spirit-daemonize-0.1.2
# spirit-log-0.1.4

* Ability to opt out of several dependencies by features (the config
  dependencies).

# spirit-log-0.1.3

* Some more logging formats.

# 0.2.6

* Don't add needless context to top-level errors.

# 0.2.5

* More detailed errors from configuration loading & friends (they have causes
  and contexts).
* Moved few utility functions into `utils` (and re-exporting them for
  compatibility).

# 0.2.4

* Turn the passed configuration paths to absolute so they survive daemonization
  or other changes of current directory.
* Utility function to parse PathBuf into absolute one in StructOpt (to allow
  custom fragments to do the same).
* Fix of the error message when missing configuration files.

# 0.2.3

* `log_errors_named`.
* Fix/improvement of the target for errors inside `run`.

# spirit-log-0.1.2

* Possibility to choose the logging format, at least a bit for now.

# spirit-daemonize-0.1.1

* Fix documentation (the key is `pid-file`, with a dash).

# 0.2.2

* Fixed matching environment variables with underscore.

# spirit-log-0.1.1

* Use standard date format (rfc3339).
* Allows choosing between local and UTC time and configuring the time format.

# 0.2.1

* Helpers for immutable config fragments.

# 0.2.0
# spirit-daemonize-0.1.0
# spirit-hyper-0.2.0
# spirit-log-0.1.0
# spirit-tokio-0.2.0

* Config hook gets access to the command line options too.
* Logging extracted to a separate helper crate (`spirit-log`).
* Daemonization extracted to a separate helper crate (`spirit-daemonize`).
* Ability to not start the background thread.
* Dropped the `S` type parameter. It now keeps the config internally, but if
  exporting to a global variable is needed, a `helpers::cfg_store` helper is
  provided.

# spirit-hyper 0.1.0

* Initial release, minimal hyper support.

# spirit-tokio 0.1.1

* The ResourceMaker trait to reuse lower-level things in higher-level
  abstractions. To be used by other helper crates.

# 0.1.1

* Link/documentation fixes.
* Added support for named groups and users when dropping privileges (thanks to
  myrrlyn).

# 0.1.0

* Inclusion of the spirit-tokio helper
* Initial implementation
