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
