//! A termination RAII guard for test purposes.
//!
//! See the [testing guide] for details.
//!
//! testing guide: crate::guide::testing

use std::sync::Arc;
use std::thread::{self, JoinHandle};

use serde::de::DeserializeOwned;
use structopt::StructOpt;

use crate::{AnyError, Spirit};

/// The termination RAII guard for test purposes.
///
/// See the [testing guide] for details of use. Created by
/// [App::run_test][run_test].
///
/// Note that this will shut down (call `terminate`) when dropped and wait for the termination to
/// happen. It'll then check that everything went successfully and panic if not. This is meant for
/// tests, so it's desired behaviour.
///
/// # Panics
///
/// The **destructor** may panic if the contained spirit app fails during termination. See above.
///
/// [testing guide]: crate::guide::testing
/// [run_test]: crate::app::App::run_test
pub struct TerminateGuard<O, C>
where
    C: DeserializeOwned + Send + Sync,
    O: StructOpt,
{
    spirit: Arc<Spirit<O, C>>,
    bg: Option<JoinHandle<Result<(), AnyError>>>,
}

impl<O, C> TerminateGuard<O, C>
where
    C: DeserializeOwned + Send + Sync,
    O: StructOpt,
{
    pub(crate) fn new(spirit: Arc<Spirit<O, C>>, bg: JoinHandle<Result<(), AnyError>>) -> Self {
        Self {
            spirit,
            bg: Some(bg),
        }
    }

    /// Access to the managed [Spirit] instance.
    pub fn spirit(&self) -> &Arc<Spirit<O, C>> {
        &self.spirit
    }
}

impl<O, C> Drop for TerminateGuard<O, C>
where
    C: DeserializeOwned + Send + Sync,
    O: StructOpt,
{
    fn drop(&mut self) {
        self.spirit.terminate();
        let result = self
            .bg
            .take()
            .expect("Drop called multiple times?!? Missing the join handle")
            .join();
        if !thread::panicking() {
            result
                .expect("Spirit test thread panicked")
                .expect("Test spirit terminated with an error");
        }
    }
}
