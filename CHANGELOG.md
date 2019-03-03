# spirit-unreleased

* `on_terminate` takes `FnOnce` instead of `FnMut`.
* `log_error!` macro, to cut down on boilerplate of the same-named function.
* `ErrorLogFormat::Multiline` replaced by `MultiLine` (inconsistency, the old
  one still stays as a deprecated alias).

TODO: Release -tokio and -hyper too, switch to the not deprecated variant.

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
