#![doc(
    html_root_url = "https://docs.rs/spirit/0.1.0/spirit/",
    test(attr(deny(warnings)))
)]
// #![deny(missing_docs, warnings)] XXX

extern crate arc_swap;
#[macro_use]
extern crate config;
extern crate failure;
extern crate serde;
#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate structopt;

use std::marker::PhantomData;
use std::path::PathBuf;

use arc_swap::ArcSwap;
use failure::Error;
use serde::Deserialize;
use structopt::StructOpt;

#[derive(Deserialize)]
struct ConfigWrapper<C> {
    #[serde(flatten)]
    config: C,
}

#[derive(Debug, StructOpt)]
struct OptWrapper<O> {
    /// Don't go into background and output logs to stderr as well.
    #[structopt(short = "f", long = "foreground")]
    foreground: bool,

    /// Configuration files or directories to load.
    #[structopt(parse(from_os_str))]
    configs: Vec<PathBuf>,
}

pub struct Spirit<'c, O = (), C: 'c = ()> {
    config: &'c ArcSwap<C>,
    opts: O,
}

impl<'c, O, C> Spirit<'c, O, C>
where
    for<'de> C: Deserialize<'de> + Send + Sync + 'c,
    O: StructOpt,
{
    #[cfg_attr(feature = "cargo-clippy", allow(new_ret_no_self))]
    pub fn new(config: &ArcSwap<C>) -> Builder<O, C> {
        Builder {
            config,
            opts: PhantomData,
        }
    }
}

pub struct Builder<'c, O, C: 'c> {
    config: &'c ArcSwap<C>,
    opts: PhantomData<O>,
}

impl<'c, O, C> Builder<'c, O, C>
where
    for<'de> C: Deserialize<'de> + Send + Sync + 'c,
    O: StructOpt,
{
    pub fn build(&self) -> Result<Spirit<'c, O, C>, Error> {
        let opts = O::from_args();
        Ok(Spirit {
            config: self.config,
            opts,
        })
    }
}
