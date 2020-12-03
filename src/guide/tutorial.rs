/*!
A tutorial

# The task to tackle

Let's go, step by step, over how an application that uses `spirit` might be built and what
everything the library can offer.

Imagine you work for a company and it is business critical to have a Hello World service. It would
listen on an HTTP endpoint and greet anyone who comes. One couldn't ask for an easier task, as
that's basically the [hyper's example](https://hyper.rs/).

But then, you show it to your boss. The boss is happy with the functionality, overall, but starts
asking pointed questions about how to configure the port it listens on, or if it can listen on
multiple at once. And where the logs go, similarly about metrics, etc, etc. Overall, the actual
functionality is fine, but there's a lot of absolutely boring boilerplate to write.

# The initial conversion, introducing spirit

That's what `spirit` is about, to cut down on it a little bit. So, let's start with first
converting the application to `spirit`, without actually using anything from it.

```rust,no_run
use std::convert::Infallible;
use std::net::SocketAddr;

use hyper::{Body, Request, Response, Server};
use hyper::service::{make_service_fn, service_fn};
use serde::Deserialize;
use spirit::prelude::*;
use spirit::Spirit;
use structopt::StructOpt;

// This'll hold our configuration. It's empty for now.
#[derive(Clone, Debug, Default, Deserialize)]
struct Cfg {

}

// Here we will have command line options. We will also add them later on. The doc string is used
// as part of the help text of the application.
/// The hello world service.
///
/// Greets people over HTTP.
#[derive(Clone, Debug, StructOpt)]
// We can use all the structopts tricks here.
#[structopt(
    version = "1.0.0-example",
    author,
)]
struct Opts {

}

async fn handle(_: Request<Body>) -> Result<Response<Body>, Infallible> {
    Ok(Response::new("Hello, World!".into()))
}

#[tokio::main]
async fn main() {
    // This'll load our configuration and options. But we didn't add any yet, so we'll just leave
    // it this way.
    let _spirit = Spirit::<Opts, Cfg>::new()
        // The false asks not to have a background thread, we'll change that later.
        .build(false)
        // Don't worry, we'll deal with this unwrap later too.
        .unwrap();

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));

    let make_svc = make_service_fn(|_conn| async {
        Ok::<_, Infallible>(service_fn(handle))
    });

    let server = Server::bind(&addr).serve(make_svc);

    if let Err(e) = server.await {
        eprintln!("server error: {}", e);
    }
}
```

This doesn't do much. What we have added is configuration line parsing. It'll be able to accept
paths to config files (yes, possibly multiple, or directories, that are scanned for the
configuration files). The configuration is then loaded ‒ from the files, and from overrides from
the command line options. We could instruct it take some environment variables or to embed a
default configuration „file“ into the program directly. And then we could access the command line
options and the configuration ‒ though the structures for them are empty for now.

We can, of course, add whatever configuration options we like, as long as they can be deserialized
using [`serde`](https://serde.rs). We could then either read the configuration from the `spirit`
object, or hook into the changes with for example
[`on_config`][crate::extension::Extensible::on_config]. This is what can be used for options
specific for our application. But we are actually worried about the parts that belong to every
service, not just our own, so we are going to use few more bits of `spirit` for that.

# Configuring logging

Let's add logging into the application first. There are two parts. One is instrumenting our code
with all the logging macros from the [`log`] crate, link `warn` and `debug`. There's nothing
unusual here.

The other part is the logger ‒ something that formats and sends the log *somewhere*. The
[`spirit-log`] crate can create the logger from configuration and command line options. So we go to
the crate's documentation and get inspired by the example there. We add the configuration options
([`fragment`][crate::fragment::Fragment]) to our own configuration structure and install the
[`pipeline`][crate::fragment::pipeline::Pipeline] that installs the logger and updates it on
configuration reloading (it'll even reopen log files on `SIGHUP`).

```rust,no_run
use std::convert::Infallible;
use std::net::SocketAddr;

use hyper::{Body, Request, Response, Server};
use hyper::service::{make_service_fn, service_fn};
use log::debug;
use serde::Deserialize;
use spirit::prelude::*;
use spirit::{Pipeline, Spirit};
use spirit_log::{Cfg as Logging, CfgAndOpts as LogBoth, Opts as LogOpts};
use structopt::StructOpt;

#[derive(Clone, Debug, Default, Deserialize)]
struct Cfg {
    /// The logging.
    ///
    /// This allows multiple logging destinations in parallel, configuring the format, timestamp
    /// format, destination.
    #[serde(default, skip_serializing_if = "Logging::is_empty")]
    logging: Logging,
}

/// The hello world service.
///
/// Greets people over HTTP.
#[derive(Clone, Debug, StructOpt)]
#[structopt(
    version = "1.0.0-example",
    author,
)]
struct Opts {
    // Adds the `--log` and `--log-module` options.
    #[structopt(flatten)]
    logging: LogOpts,
}

async fn handle(req: Request<Body>) -> Result<Response<Body>, Infallible> {
    debug!("Handling request {:?}", req);
    Ok(Response::new("Hello, World!".into()))
}

#[tokio::main]
async fn main() {
    let _spirit = Spirit::<Opts, Cfg>::new()
        // This is the new, interesting part. It takes both the logging configuration and command
        // line options, puts them together and tells system put it in place.
        //
        // Note that it would allow us to augment the logger on the way through the pipeline (for
        // example adding a hardcoded logger to the set provided by the configuration ‒ it can be
        // used for example to integrate with sentry, if you have that need).
        .with(
            Pipeline::new("logging").extract(|opts: &Opts, cfg: &Cfg| LogBoth {
                cfg: cfg.logging.clone(),
                opts: opts.logging.clone(),
            }),
        )
        .build(false)
        .unwrap();

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));

    let make_svc = make_service_fn(|_conn| async {
        Ok::<_, Infallible>(service_fn(handle))
    });

    let server = Server::bind(&addr).serve(make_svc);

    if let Err(e) = server.await {
        eprintln!("server error: {}", e);
    }
}
```

So we can now configure the log destinations in a config file (and we can set multiple ones, with
different logging levels, at once, filtering of log levels based on the log target, and some more).
Once we turn on the background thread, we'll gain ability to change logging at runtime. It comes
handy, but having to write it every time into each new application is tedious ‒ now we can reuse it
every time.

Similarly, we can pull in support for configuring metrics, a HTTP client, proper daemonization
(though it is no longer as fashionable for services to go into background on their own and some
 kind of init system thing usually handles that), etc. Hopefully, as time goes, there'll be even
more.

All these crates provide their own bits of configuration and an example that can be used as the
basic „canonical“ way to use them and tweak it to get exactly the needed support.

# Improving the UX around configuration

So we have pulled in a whole bunch of configuration fragments and we are not stopping here, we are
going to configure our own things too in a moment. And we can compose the final configuration from
multiple files, environment variables, etc. That's great, but it also means we can easily get lost
in the sheer amount of things that can be configured and knowing what configuration we run with is
also a bit of a challenge.

This is where [`spirit-cfg-helpers`] come into play. This allows us, with a bit of work, to add few
new command line switches. The `--help-config` flag is similar to help ‒ it prints the whole tree
of what *can* be configured, each option with a little desciption (yes, we'll talk about where that
comes from). The `--dump-config` does the start up of the application up to the point when the
configuration is composed from all the parts, parsed and then it just prints the whole
configuration as it *would have* been used and exits.

Apart from having to throw the right field into our configuration option structure and plugging it
in, like with the other crates, we need make sure our configuration structure implements two
additional traits.

The [`Serialize`][serde::Serialize] allows us to take the parsed configuration structure and dump
it. Usually, it can be simply derived (and it pairs with the [`Deserialize`][serde::Deserialize]
use to parse the configuration in the first place).

If you're feeling perfectionist, you can optimize the behaviour of the parsing and dumping for
better UX. For example, annotating vectors of things with `#[serde(default, skip_serializing_if =
"Vec::is_empty")]` makes the vector disappear if it has no elements. Similar with [`Option`]. There
are few utilities around, like [`deserialize_duration`][crate::utils::deserialize_duration] (so you
can specify durations in form of `3days 5hours`). There's also the [`Hidden`][crate::utils::Hidden]
to hide passwords from the dump and logs ‒ if you have a field `password: Hidden<Stryng>`, it'll be
printed to both as `***` instead the actual password.

The [`StructDoc`][structdoc::StructDoc] provides the help and the structure of the configuration
file. It is derived in a similar way to the [`serde`] derives. The help is taken from the doc
comments. If you use a type that is not known to it, you can annotate it with `#[structdoc(leaf =
"name-of-type")]` and the type in the help will be taken from there.

The support for `structdoc` is optional in the spirit extension crates, under the `cfg-help`
feature, but turned on by default.  If you disable default features, you may need to turn the
`structdoc` support on explicitly in `Cargo.toml`.

```toml
...
spirit-log = { version = "0.4", default-features = false, features = ["cfg-help"] }
```

So let's extend our example:

```rust,no_run
use std::convert::Infallible;
use std::net::SocketAddr;

use hyper::{Body, Request, Response, Server};
use hyper::service::{make_service_fn, service_fn};
use log::debug;
use serde::{Deserialize, Serialize};
use spirit::prelude::*;
use spirit::{Pipeline, Spirit};
use spirit_log::{Cfg as Logging, CfgAndOpts as LogBoth, Opts as LogOpts};
use structdoc::StructDoc;
use structopt::StructOpt;

// We have just added the two new derives in here
#[derive(Clone, Debug, Default, Deserialize, Serialize, StructDoc)]
struct Cfg {
    /// The logging.
    ///
    /// This allows multiple logging destinations in parallel, configuring the format, timestamp
    /// format, destination.
    #[serde(default, skip_serializing_if = "Logging::is_empty")]
    logging: Logging,
}

/// The hello world service.
///
/// Greets people over HTTP.
#[derive(Clone, Debug, StructOpt)]
// We can use all the structopts tricks here.
#[structopt(
    version = "1.0.0-example",
    author,
)]
struct Opts {
    // Adds the `--log` and `--log-module` options.
    #[structopt(flatten)]
    logging: LogOpts,
}

async fn handle(req: Request<Body>) -> Result<Response<Body>, Infallible> {
    debug!("Handling request {:?}", req);
    Ok(Response::new("Hello, World!".into()))
}

#[tokio::main]
async fn main() {
    let _spirit = Spirit::<Opts, Cfg>::new()
        // This is the new, interesting part. It takes both the logging configuration and command
        // line options, puts them together and tells system put it in place.
        //
        // Note that it would allow us to augment the logger on the way through the pipeline (for
        // example adding a hardcoded logger to the set provided by the configuration ‒ it can be
        // used for example to integrate with sentry, if you have that need).
        .with(
            Pipeline::new("logging").extract(|opts: &Opts, cfg: &Cfg| LogBoth {
                cfg: cfg.logging.clone(),
                opts: opts.logging.clone(),
            }),
        )
        .build(false)
        .unwrap();

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));

    let make_svc = make_service_fn(|_conn| async {
        Ok::<_, Infallible>(service_fn(handle))
    });

    let server = Server::bind(&addr).serve(make_svc);

    if let Err(e) = server.await {
        eprintln!("server error: {}", e);
    }
}
```

# Configuring the web server and managing the whole lifetime

Until now, we just created the `spirit` object at the beginning, let it initialize stuff, but then
forgot about it. That's not taking the full advantage of it. What the thing can do for us is also
to reload configuration in the background, terminate when asked for, run shutdown tasks, and handle
errors during startup.

But for that we need to integrate the HTTP server into `spirit`, so it can be manipulated by it.
Spirit provides configuration fragments both for the [`tokio`] runtime (configuring number of
threads, for example ‒ up until now we have let it decide for us) and for HTTP servers. Let's pull
them in.

Then we move from using `build` to using `run` to execute the actual „body“ of the application.

```rust,no_run
use hyper::{Body, Request, Response};
use log::debug;
use serde::{Deserialize, Serialize};
use spirit::prelude::*;
use spirit::{Pipeline, Spirit};
use spirit_log::{Cfg as Logging, CfgAndOpts as LogBoth, Opts as LogOpts};
use spirit_hyper::{server_from_handler, BuildServer, HttpServer};
use spirit_tokio::runtime::Config as TokioCfg;
use spirit_tokio::Tokio;
use structdoc::StructDoc;
use structopt::StructOpt;

// We have just added the two new derives in here
#[derive(Clone, Debug, Default, Deserialize, Serialize, StructDoc)]
struct Cfg {
    /// The logging.
    ///
    /// This allows multiple logging destinations in parallel, configuring the format, timestamp
    /// format, destination.
    #[serde(default, skip_serializing_if = "Logging::is_empty")]
    logging: Logging,

    // Some more configuration things go in here.
    /// The work threadpool.
    ///
    /// This is for performance tuning.
    threadpool: TokioCfg,

    /// Where to listen on for incoming requests.
    // Makes the array "disappear" if empty. Omit to always force at least an explicit empty array.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    listen: Vec<HttpServer>,
}

impl Cfg {
    fn listen(&self) -> &Vec<HttpServer> {
        &self.listen
    }
}

/// The hello world service.
///
/// Greets people over HTTP.
#[derive(Clone, Debug, StructOpt)]
// We can use all the structopts tricks here.
#[structopt(
    version = "1.0.0-example",
    author,
)]
struct Opts {
    #[structopt(flatten)]
    logging: LogOpts,
}

async fn handle(req: Request<Body>) -> Response<Body> {
    debug!("Handling request {:?}", req);
    Response::new("Hello, World!".into())
}

// Note: The main is no longer async tokio::main thing, it is the ordinary blocking main.
fn main() {
    Spirit::<Opts, Cfg>::new()
        .with(
            Pipeline::new("logging").extract(|opts: &Opts, cfg: &Cfg| LogBoth {
                cfg: cfg.logging.clone(),
                opts: opts.logging.clone(),
            }),
        )
        // Right, this is not a pipeline. This serves a double purpose. First, it integrates the
        // tokio runtime into the spirit, which must be started *before* run (it sometimes can be
        // started implicitly as part of a pipeline that uses tokio, but the only one we have here
        // is plugged in inside the run).
        //
        // The other purpose is to actually configure how many threads run, etc.
        //
        // Rust gets confused if you don't provide at least `cfg: &_` as the type.
        .with(Tokio::from_cfg(|cfg: &Cfg| cfg.threadpool.clone()))
        // The run will first construct the spirit object, then run the body. If there's any error
        // both during the setup (before run) or inside run, the error is logged (to the configured
        // logging location, if that is already set up, or to stderr if it is very early).
        .run(|spirit| {
            // Here we could do some more loading, starting up, etc.
            // We also can access the spirit and read configuration from it.
            //
            // But actually, we just plug another pipeline in, one that sets up the http server(s).
            // We could do that before run too, but this way all the theoretical loading happens
            // before we start listening and we can also use that and pass it to the created
            // server.

            spirit.with(
                Pipeline::new("listen")
                    // Yes, we are passing the whole vector to spirit, but provide instruction how
                    // to create one server. Spirit figures on its own how to start multiple from
                    // that, which ones to add or remove on a change and all that.

                    // FIXME: Does anyone know why we need an actual function in here and inline
                    // closure is just not enough? If we just inline it here, rustc is not able to
                    // build the whole pipeline and one of the million trains don't align right,
                    // though it is confused enough not to even hint at which one. A rustc bug?
                    .extract_cfg(Cfg::listen)
                    .transform(BuildServer(server_from_handler(handle)))
            )?;

            Ok(())

            // Now, the `run` will terminate. But the spirit will wait with shutdown until the
            // futures ‒ in case the servers we started ‒ finish running.
        });
}
```

This adds a bit of copy-paste style code from docs, but we gained the ability to configure multiple
HTTP servers, including fine details like how many concurrent connections are we willing to have
open at each time, or if we want to support HTTP2. As a bonus, we get the ability to actually
reconfigure what ports (and addresses) we listen on at runtime.

There are few more tricks. It is possible to also listen on unix domain sockets, or even create a
hybrid configuration where some instances listen on IPv4, some on IPv6 and some on unix domain
sockets. Furthermore, the server configuration fragment can be parametrized by additional type
parameter. The library doesn't touch it, but it is possible ‒ if you don't use the
[`server_from_handler`][spirit_hyper::server_from_handler], but build the server manually ‒ to
access that field and customize each separate instance of the listening server.

Let's also mention a small trick. The [`Pipeline`][crate::fragment::pipeline::Pipeline] is built in
the usual builder pattern, by incrementally changing how it looks. But for that to be possible, the
traits of the types used around it may not match in the middle of the process. Therefore, the type
checking is enforced only at the very end, when being plugged into spirit. That has the downside
that if the types don't align correctly, the error message is often less helpful or less exact than
one would like. One can place the `.check()` method in various places in the pipeline construction
‒ that one forces the type check at that point, and often provides a bit more information even if
used at the very end (I'm not sure exactly why that is). If that still doesn't help, try
simplifying the pipeline (using single value instead of vector, for example) ‒ eventually, `rustc`
should be able to cope with the trait complexity and provide something useful.

# The complete thing

The repository contains
[an example](https://github.com/vorner/spirit/blob/master/examples/hws-complete.rs) that is an
extended version of the above exercise. You'll further find:

* Some more logging, including of the content of newly loaded configuration.
* Embedding of base configuration inside the program.
* Looking for configuration inside the environment.
* Parametrizing the behaviour of the server by both global (whole application) and local (one
  server instance) configuration.
* Support for the unix domain sockets.
* Daemonization.
* Manual building of the server, which allows for returning errors, for passing context and other a
  bit more advanced things.

Hopefully, this shows some of the possibilities of what spirit is capable of. Further chapters do
into more details about specific topics.
*/
