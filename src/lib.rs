#![doc(
    html_root_url = "https://docs.rs/spirit/0.1.0/spirit/",
    test(attr(deny(warnings)))
)]
// #![deny(missing_docs, warnings)] XXX

extern crate arc_swap;
extern crate config;
extern crate failure;
extern crate serde;
#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate structopt;

use std::fmt::Debug;
use std::marker::PhantomData;
use std::path::PathBuf;

use arc_swap::ArcSwap;
use failure::Error;
use serde::Deserialize;
use structopt::StructOpt;
use structopt::clap::App;

#[derive(Deserialize)]
struct ConfigWrapper<C> {
    #[serde(flatten)]
    config: C,
}

#[derive(Debug, StructOpt)]
struct CommonOpts {
    /// Don't go into background and output logs to stderr as well.
    #[structopt(short = "f", long = "foreground")]
    foreground: bool,

    /// Configuration files or directories to load.
    #[structopt(parse(from_os_str))]
    configs: Vec<PathBuf>,

}

#[derive(Debug)]
struct OptWrapper<O> {
    common: CommonOpts,
    other: O,
}

// Unfortunately, StructOpt doesn't like flatten with type parameter
// (https://github.com/TeXitoi/structopt/issues/128). It is not even trivial to do, since some of
// the very important functions are *not* part of the trait. So we do it manually ‒ we take the
// type parameter's clap definition and add our own into it.
impl<O> StructOpt for OptWrapper<O>
where
    O: Debug + StructOpt,
{
    fn clap<'a, 'b>() -> App<'a, 'b> {
        CommonOpts::augment_clap(O::clap())
    }
    fn from_clap(matches: &::structopt::clap::ArgMatches) -> Self {
        OptWrapper {
            common: StructOpt::from_clap(matches),
            other: StructOpt::from_clap(matches),
        }
    }
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
    O: Debug + StructOpt,
{
    pub fn build(&self) -> Result<Spirit<'c, O, C>, Error> {
        let opts = OptWrapper::<O>::from_args();
        Ok(Spirit {
            config: self.config,
            opts: opts.other,
        })
    }
}
