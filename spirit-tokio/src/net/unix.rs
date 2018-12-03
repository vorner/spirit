use std::fmt::Debug;
use std::os::unix::net::{UnixDatagram as StdUnixDatagram, UnixListener as StdUnixListener};
use std::path::PathBuf;

use failure::Error;
use spirit::Empty;
use spirit::validation::Results as ValidationResults;
use tokio::net::unix::{Incoming, UnixDatagram, UnixListener, UnixStream};
use tokio::reactor::Handle;

use base_traits::{ExtraCfgCarrier, ResourceConfig};
use scaled::{Scale, Scaled};
use net::{ConfiguredStreamListener, IntoIncoming, StreamConfig, WithListenLimits};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(rename_all = "kebab-case")]
pub struct Listen {
    path: PathBuf,
    // TODO: Permissions
    // TODO: Remove
}

impl Listen {
    pub fn create_listener(&self) -> Result<StdUnixListener, Error> {
        StdUnixListener::bind(&self.path).map_err(Error::from)
    }
    pub fn create_datagram(&self) -> Result<StdUnixDatagram, Error> {
        StdUnixDatagram::bind(&self.path).map_err(Error::from)
    }
}

pub type UnixConfig = Empty;

impl IntoIncoming for UnixListener {
    type Connection = UnixStream;
    type Incoming = Incoming;
    fn into_incoming(self) -> Incoming {
        self.incoming()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct UnixListen<ExtraCfg = Empty, ScaleMode = Scale, UnixStreamConfig = UnixConfig> {
    #[serde(flatten)]
    listen: Listen,
    #[serde(flatten)]
    scale: ScaleMode,
    #[serde(flatten)]
    extra_cfg: ExtraCfg,
    #[serde(flatten)]
    unix_config: UnixStreamConfig,
}

impl<ExtraCfg, ScaleMode, UnixStreamConfig> ExtraCfgCarrier
    for UnixListen<ExtraCfg, ScaleMode, UnixStreamConfig>
{
    type Extra = ExtraCfg;
    fn extra(&self) -> &ExtraCfg {
        &self.extra_cfg
    }
}

impl<ExtraCfg, ScaleMode, UnixStreamConfig, O, C> ResourceConfig<O, C>
    for UnixListen<ExtraCfg, ScaleMode, UnixStreamConfig>
where
    ExtraCfg: Debug + PartialEq + Send + Sync + 'static,
    ScaleMode: Debug + PartialEq + Scaled + Send + Sync + 'static,
    UnixStreamConfig: Clone + Debug + PartialEq + Send + Sync + StreamConfig<UnixStream> + 'static,
{
    type Seed = StdUnixListener;
    type Resource = ConfiguredStreamListener<UnixListener, UnixStreamConfig>;
    fn create(&self, _: &str) -> Result<StdUnixListener, Error> {
        self.listen.create_listener()
    }
    fn fork(&self, seed: &StdUnixListener, _: &str) -> Result<Self::Resource, Error> {
        let config = self.unix_config.clone();
        seed.try_clone() // Another copy of the listener
            // std → tokio socket conversion
            .and_then(|listener| UnixListener::from_std(listener, &Handle::default()))
            .map_err(Error::from)
            .map(|listener| ConfiguredStreamListener { listener, config })
    }
    fn scaled(&self, name: &str) -> (usize, ValidationResults) {
        self.scale.scaled(name)
    }
    fn is_similar(&self, other: &Self, _: &str) -> bool {
        self.listen == other.listen
    }
}

pub type MinimalUnixListen<ExtraCfg = Empty> = UnixListen<ExtraCfg, Empty, Empty>;

pub type UnixListenWithLimits<ExtraCfg = Empty, ScaleMode = Scale, UnixStreamConfig = UnixConfig> =
    WithListenLimits<UnixListen<ExtraCfg, ScaleMode, UnixStreamConfig>>;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct DatagramListen<ExtraCfg = Empty, ScaleMode: Scaled = Scale> {
    #[serde(flatten)]
    listen: Listen,
    #[serde(flatten)]
    scale: ScaleMode,
    #[serde(flatten)]
    extra_cfg: ExtraCfg,
}

impl<ExtraCfg, ScaleMode> ExtraCfgCarrier for DatagramListen<ExtraCfg, ScaleMode>
where
    ScaleMode: Scaled,
{
    type Extra = ExtraCfg;
    fn extra(&self) -> &ExtraCfg {
        &self.extra_cfg
    }
}

impl<ExtraCfg, ScaleMode, O, C> ResourceConfig<O, C> for DatagramListen<ExtraCfg, ScaleMode>
where
    ExtraCfg: Debug + PartialEq + Send + Sync + 'static,
    ScaleMode: Debug + PartialEq + Scaled + Send + Sync + 'static,
{
    type Seed = StdUnixDatagram;
    type Resource = UnixDatagram;
    fn create(&self, _: &str) -> Result<StdUnixDatagram, Error> {
        self.listen.create_datagram()
    }
    fn fork(&self, seed: &StdUnixDatagram, _: &str) -> Result<UnixDatagram, Error> {
        seed.try_clone() // Another copy of the socket
            // std → tokio socket conversion
            .and_then(|socket| UnixDatagram::from_std(socket, &Handle::default()))
            .map_err(Error::from)
    }
    fn scaled(&self, name: &str) -> (usize, ValidationResults) {
        self.scale.scaled(name)
    }
    fn is_similar(&self, other: &Self, _: &str) -> bool {
        self.listen == other.listen
    }
}

cfg_helpers! {
    impl helpers for UnixListen <ExtraCfg, ScaleMode, UnixStreamConfig>
    where
        ExtraCfg: Debug + PartialEq + Send + Sync + 'static,
        ScaleMode: Debug + PartialEq + Scaled + Send + Sync + 'static,
        UnixStreamConfig: Debug + PartialEq + Send + Sync + 'static;

    impl helpers for DatagramListen <ExtraCfg, ScaleMode>
    where
        ExtraCfg: Debug + PartialEq + Send + Sync + 'static,
        ScaleMode: Debug + PartialEq + Scaled + Send + Sync + 'static;
}
