/*!
Extending Spirit

Eventually, you may want to create your own [`Extension`]s and [`Fragment`]s, for more convenient
reusability.

# Extensions

[`Extension`]s are just glorified functions. They get a [`Builder`], modify it and return it back.
They, however, can be plugged in conveniently by the
[`with`][crate::extension::Extensible::with]. You can create an extension to plug in
configuration to a part of application, or a reusable
library.

```rust
use spirit::{Builder, Empty, Spirit};
use spirit::prelude::*;

fn ext<O, C>(builder: Builder<O, C>) -> Builder<O, C> {
    builder.on_terminate(|| println!("I'm done here"))
}

Spirit::<Empty, Empty>::new().with(ext).run(|_| Ok(()));
```

# Fragments

A fragment is a bit of configuration that can create *something*. They are to be used through
[`Pipeline`]s and that pipeline will become an extension, so it can be registered just like one.

That happens in two phases (a `Seed` and a `Resource`). This allows for creating resources that
actually allocate something unique in the OS (maybe a socket listening on a port) and connect it
with some further configuration. If only the additional configuration is changed, only the second
phase is run. This works even if the reconfiguration fails ‒ it doesn't require relinquishing the
original in the attempt.

Nevertheless, most of the times one doesn't need both instances. It is enough to specify the driver
(see below), a function to create that something and provide a type that puts it to use (an
[`Installer`]). For that, you can use the [`simple_fragment`] macro.

# Drivers

Most of the time, if configuration is reloaded, most of it stays the same. But replacing everything
inside the application may be a bit expensive and therefore wasteful. So pipelines can decide when
it makes sense to re-run either both or just the second phase or if whatever they manage shall stay
the same.

Drivers are what manages this context and makes the decision. There's a selection of them in the
[`drivers`][crate::fragment::driver]. The fragment chooses the default driver for it, but the user
can override it.

Note that composite fragments (eg. `Vec<F: Fragment>`) also compose their drivers, to make the
composite types work ‒ the outer (the driver for the `Vec`) keeps track and is able to add and
remove instances as needed.

Note that for a fragment to participate in this composition, one need to implement the relevant
marker traits ([`Stackable`][crate::fragment::Stackable], [`Optional`][crate::fragment::Optional].

# Installers

At the end of the pipeline, there needs to be a way to put the resource to use or to withdraw it if
it no longer should exist according to the documentation. That's done by the [`Installer`]. It can
install it into a global place (for example the logger is installed into a global place, and is
 *not* `Stackable` by the way). Some others install into a specific place (and therefore need to be
 provided by the user).

The installer returns a handle to the installed resource. By dropping the handle, the pipeline
signals that the resource should be removed.

[`Extension`]: crate::extension::Extension
[`Fragment`]: crate::fragment::Fragment
[`Builder`]: crate::Builder
[`Installer`]: crate::fragment::Installer
[`simple_fragment`]: crate::simple_fragment
[`Pipeline`]: crate::fragment::pipeline::Pipeline
*/
