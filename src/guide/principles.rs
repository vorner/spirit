/*!
The basic principles

At the core, the [`Spirit`] is the top level manager of things. It is parametrized by two
structures, one to hold parsed command line (implementing [`StructOpt`]), the other for holding the
parsed configuration options (implementing [`serde`'s `Derive`][`Deserialize`]). It parses the
command line, enriched with some options of its own. It uses these options to find the
configuration files and loads the configuration for the first time.

To manage the life time of the application, it is possible to register hooks into the manager. They
are called on certain events ‒ when new configuration is loaded (both the first time and when it is
reloaded), when the application is about to be terminated, on registered signal, etc.

It then runs the „body“ of the whole application and leaves it to run. The manager does its job in
its own background thread.

Note that many things here can be turned off (see further chapters about that) if they are not
doing what you want.

# Extensions and pipelines

Having a place to put callbacks might be handy a bit, but certainly nothing to make any fuss about
(or write tutorials about). The added value of the library is the system of
[extensions][crate::extension::Extension], [fragments][crate::fragment::Fragment] and
[pipelines][crate::fragment::pipeline::Pipeline]. There are other, related crates (eg.
`spirit-log`) that provide these and allow easy and reusable plugging of functionality in.

An extension simply modifies the [builder][crate::Builder], usually by registering some callbacks.
The fragments and pipelines are ways to build extensions from already existing parts. They are
explained in detail in [their own chapter][super::fragments].

# Error handling conventions

In a library like this, quite a lot of things can go wrong. Some of the errors might come from the
library, but many would come from user-provided code in callbacks, extensions, etc. Error are
passed around as a boxed trait objects (the [`AnyError`][crate::AnyError] is just a boxed trait
object). Other option would be something like the [`anyhow`](https://lib.rs/anyhow), but that would
force the users of the library into a specific one. Future versions might go that way if there's a
clear winner between these crates, but until then we stay with only what [`std`] provides.

It is expected that most of these errors can't be automatically handled by the application,
therefore distinguishing types of the errors isn't really a concern most of the time, though it
is possible to get away with downcasting. In practice, most of the errors will end up somewhere
in logs or other places where users can read them.

To make the errors more informative, the library constructs layered errors (or error chains).
The outer layer is the high level problem, while the inner ones describe the causes of the
problem. It is expected all the layers are presented to the user. When the errors are handled
by the library (either in termination error or with unsuccessful configuration reload), the
library prints all the layers. To replicate similar behaviour in user code, it is possible to
use the [`log_error`][macro@crate::log_error] macro or [`log_error`][fn@crate::error::log_error]
function.

Internally, the library uses the [`err-context`] crate to construct and handle such errors. In
addition to constructing such errors, the crate also allows some limited examination of error
chains. However, users are not forced to use that crate as the chains constructed are based
directly on the [`std::error::Error`] trait and are therefore compatible with errors constructed in
any other way.

[`Spirit`]: crate::Spirit
[`StructOpt`]: structopt::StructOpt
[`Deserialize`]: serde::Deserialize
[`err-context`]: https://lib.rs/crates/err-context
*/
