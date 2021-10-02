/*!
Testing

Sometimes, during tests, one might want to run an almost-full application. It can be done by
executing the compiled binary externally, but such thing is quite heavy-weight to orchestrate
(making sure it is already compiled, providing configuration, waiting for it to become ready,
shutting it down properly).

Alternatively, it is possible to get close to full application setup inside a local test function
with spirit.

First, make sure the application setup is a separate function that can be applied to the spirit
[Builder]. That'll allow the tests [Spirit] to use the same core setup as the application. The only
missing part is passing the right configuration and arguments to it (because it should *not* use
the arguments passed to the test process). To do so, the [config Builder] can be used.

We also disable the background management thread. First, we want to make sure we don't touch signal
handlers so things work as expected. Second, this allows having multiple parallel instances of the
"application" (rust tests run in parallel).

[Builder]: crate::Builder
[Spirit]: crate::Spirit
[config Builder]: crate::cfg_loader::Builder

```rust
// This goes to some tests/something.rs or similar place.
use std::sync::{mpsc, Arc};

use spirit::prelude::*;
use spirit::{Empty, Spirit};

// Custom configuration "file"
let TEST_CFG: &str = r#"
[section]
option = true
"#;

// Note: Some "real" config and opts structures are likely used here.
let app = Spirit::<Empty, Empty>::new()
    // Inject the "config file" in it.
    .config_defaults(TEST_CFG)
    // Provide already parsed command line argument structure here. Ours is `Empty` here, but it's
    // whatever you use in the app.
    //
    // This also ignores the real command line arguments, so it's important to call in the test
    // even if you pass `Default::default()`. We want to ignore the arguments passed to the test
    // binary.
    .preparsed_opts(Empty::default())
    // False -> we don't have the background management thread that listens to signals. Lifetime is
    // managed in a different way here in the test.
    .build(false)
    .expect("Failed to create the test spirit app");

// We want to wait for the background thread to fully start up as necessary so the test doesn't do
// its stuff too early. We'll let the application to signal us.
let (ready_send, ready_recv) = mpsc::channel::<()>();

// A spirit copy to go inside the closure below.
let spirit = Arc::clone(app.spirit());

// The running is a RAII guard. Once it's dropped, it will terminate spirit and check it terminated
// correctly. Runs the application in another thread.
let running = app.run_test(move || {
    // Once everything is started up enough for the test to start doing stuff, we'll tell it to
    // proceed (ignore errors in case the test somehow died).
    let _ = ready_send.send(());
    // The inside of the application. You should have a function that goes inside `run` and
    // call it both here and from the real `main`'s `run`.
    while !spirit.is_terminated() {
        // Do stuff!
    }
    Ok(())
});

// Wait for the background thread to be ready (or dead, which would error out and the test would
// fail).
let _ = ready_recv.recv();

// Now, we can do our testing here. After we are done, the above will just shut down all the stuff.
assert!(!running.spirit().is_terminated());
```
*/
