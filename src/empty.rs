use serde::{Deserialize, Serialize};
use structopt::StructOpt;

/// A struct that may be used when either configuration or command line options are not needed.
///
/// When the application doesn't need the configuration (in excess of the automatic part provided
/// by this library) or it doesn't need any command line options of its own, this struct can be
/// used to plug the type parameter.
///
/// Other places (eg. around helpers) may use this to plug a type parameter that isn't needed, do
/// nothing or something like that.
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Deserialize,
    Eq,
    PartialEq,
    Hash,
    Ord,
    PartialOrd,
    StructOpt,
    Serialize,
)]
#[cfg_attr(feature = "cfg-help", derive(structdoc::StructDoc))]
pub struct Empty {}

