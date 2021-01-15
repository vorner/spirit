/*!
The user guide

*This module doesn't contain any APIs or code. It contains only the documentation.*

In short, when writing a daemon or a service, we have the *"muscle"* of the
application ‒ whatever we write the daemon for. And we have a whole lot of
infrastructure around that: logging, command line parsing, configuration, to
name a few. While there are Rust libraries for all that, it requires a
non-trivial amount of boilerplate code to bridge all this together.

Spirit aims to be this bridge.

It takes care of things like

- signal handling and the application lifecycle
- combining multiple pieces of configuration together with command line and environment variables
  overrides
- it allows for reloading the configuration at runtime
- application metrics

It leverages the existing crate ecosystem and provides the plumbing to connect all the necessary
pieces together. Nevertheless, it specifically aims *not* to be a framework. While there is a
certain way the library is designed to work, it isn't *required* to be used that way and should
gracefully get out of your way at places where you want to do something in your own way, or to
perform only part of the management.

You'll find here:

* [Description of basic principles][self::principles]
* [Lower and higher levels of API][self::levels]
* [A tutorial][self::tutorial]
* Common tasks
  - [Loading & handling configuration][self::configuration]
* Advanced topis:
  - [Using fragments and pipelines][self::fragments]
  - [Extending with own fragments][self::extend]
  - [Proper daemonization and early startup][self::daemonization]
*/

pub mod configuration;
pub mod daemonization;
pub mod extend;
pub mod fragments;
pub mod levels;
pub mod principles;
pub mod tutorial;
