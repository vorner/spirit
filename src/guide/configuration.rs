/*!
Loading and handling configuration

Apart from the fact that one doesn't have to use the whole [`Spirit`][crate::Spirit] to load
configuration, you can use the [`Loader`][crate::cfg_loader::Loader] if you don't need the rest of
the stuff, there's nothing interesting about it. Or, is there? Well, there's a lot of little
details you might want to use.

# Sources of configuration

The configuration comes from different sources and is merged together. The last one to set a value
wins.

* A configuration embedded inside the program through
  [config_defaults][crate::cfg_loader::ConfigBuilder::config_defaults].
* Configuration loaded from files. They are loaded in the order they appear on the command line. If
  it specifies a directory, all config files in there are loaded, in the order of sorting (the list
  is made anew at each reload). If no path is set on the command line, the value set through
  [config_default_paths][crate::cfg_loader::ConfigBuilder::config_default_paths].
* Things loaded from environment variables, if a [prefix is
  configured][crate::cfg_loader::ConfigBuilder::config_env].
* Last, the values set on command line through `--config-override`, are used.

# Configuration phases

Every time the configuration is loaded, it goes through few phases. If any of them fails, the whole
loading fails. If it's the first time, the application aborts. On any future reload, if it fails,
the error is logged, but the application continues with previous configuration.

* First, the configuration is composed together and merged.
* It is deserialized with [`serde`] and the configuration is filled into the provided structure.
* The [validators][crate::extension::Extensible::config_validator] are run. They can schedule an
  action to run on commit, when everything succeeds, or on abort, if it fails. It allows to either
  postpone actually activating the configuration, or rolling back an attempt. Only once all of them
  succeed, the new configuration is activated.
* Then the [`on_config`][crate::extension::Extensible::on_config] hooks are run.

# Using the configuration

One can use the configuration in multiple ways:

* From the validator, try it out and schedule using it or throwing it out. This is useful if the
  configuration may still be invalid even if it is the right type.
* With the `on_config` hook, if it can't possibly fail.
* Look into it each time it is needed. There is the [`config`][crate::Spirit::config] method.

A variation of the last one is propagating it through parts of program through [`arc-swap`]. It
also have the [`access`][arc_swap::access] module, to „slice“ the shared configuration and provide
only part of it to different parts of program.

```rust
# use std::sync::Arc;
#
# use arc_swap::ArcSwap;
# use serde::Deserialize;
# use spirit::{Empty, Spirit};
# use spirit::prelude::*;
#
# fn some_part_of_app<T>(_: T) {}
#[derive(Clone, Debug, Default, Deserialize)]
struct Cfg {
    // Something goes in here
}

let cfg_store = Arc::new(ArcSwap::from_pointee(Cfg::default()));

Spirit::<Empty, Cfg>::new()
    .on_config({
        let cfg_store = Arc::clone(&cfg_store);
        move |_, cfg| cfg_store.store(cfg.clone())
    })
    .run(|_| {
        some_part_of_app(cfg_store);
        Ok(())
    });
```
*/
