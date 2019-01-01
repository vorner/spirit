use std::process;
use std::sync::Arc;

use failure::Error;
use log::debug;

use crate::bodies::{InnerBody, WrapBody};
use crate::spirit::Spirit;
use crate::utils;

pub struct App<O, C> {
    spirit: Arc<Spirit<O, C>>,
    inner: InnerBody,
    wrapper: WrapBody,
}

impl<O, C> App<O, C>
where
    O: Send + Sync + 'static,
    C: Send + Sync + 'static,
{
    pub(crate) fn new(spirit: Arc<Spirit<O, C>>, inner: InnerBody, wrapper: WrapBody) -> Self {
        Self {
            spirit,
            inner,
            wrapper,
        }
    }
    pub fn spirit(&self) -> &Arc<Spirit<O, C>> {
        &self.spirit
    }

    pub fn run<B>(self, body: B) -> Result<(), Error>
    where
        B: FnOnce() -> Result<(), Error> + Send + 'static,
    {
        debug!("Running bodies");
        let inner = self.inner;
        let inner = move || inner.run().and_then(|()| body());
        self.wrapper.run(InnerBody(Box::new(Some(|()| inner()))))
    }

    pub fn run_term<B>(self, body: B)
    where
        B: FnOnce() -> Result<(), Error> + Send + 'static,
    {
        if utils::log_errors_named("top-level", || self.run(body)).is_err() {
            process::exit(1);
        }
    }
}
