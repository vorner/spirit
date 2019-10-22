# Overview

In short, when writing a daemon or a service, we have the *"muscle"* of the
application ‒ whatever we write the daemon for. And we have a whole lot of
infrastructure around that: logging, command line parsing, configuration, to
name a few. While there are Rust libraries for all that, it requires a
non-trivial amount of boilerplate code to bridge all this together.

Spirit aims to be this bridge. 

It takes care of things like

- signal handling and the application lifecycle
- combining multiple pieces of configuration together with command line
  overrides
- it allows for reloading the configuration at runtime
- application metrics

In short, it leverages the existing crates ecosystem to provide developers
with a batteries-included approach to writing daemons. You provide the
application and we give you the tools needed to create a production-ready
daemon with little effort, while still letting you bend the framework
to suit your specific use-case when necessary.

