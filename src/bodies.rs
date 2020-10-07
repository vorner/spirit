use std::sync::Arc;

use crate::{AnyError, Spirit};

pub(crate) type InnerBody = Box<dyn FnOnce() -> Result<(), AnyError> + Send>;
pub(crate) type WrapBody = Box<dyn FnOnce(InnerBody) -> Result<(), AnyError> + Send>;
pub(crate) type Wrapper<O, C> =
    Box<dyn FnOnce(&Arc<Spirit<O, C>>, InnerBody) -> Result<(), AnyError> + Send>;
pub(crate) type SpiritBody<O, C> =
    Box<dyn FnOnce(&Arc<Spirit<O, C>>) -> Result<(), AnyError> + Send>;
pub(crate) type HookBody<'a> = Box<dyn FnOnce() + 'a>;
pub trait HookWrapper: for<'a> FnMut(HookBody<'a>) + Send + 'static {}

impl<F> HookWrapper for F where F: for<'a> FnMut(HookBody<'a>) + Send + 'static {}

#[cfg(test)]
mod tests {
    use super::*;

    fn _accept_hook_wrapper(_: Box<dyn HookWrapper>) {}

    // Not really run, we check this compiles
    fn _hook_wrapper_works() {
        _accept_hook_wrapper(Box::new(|inner| inner()));
    }
}
