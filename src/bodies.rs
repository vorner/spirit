use std::sync::Arc;

use crate::{AnyError, Spirit};

pub(crate) type InnerBody = Box<dyn FnOnce() -> Result<(), AnyError> + Send>;
pub(crate) type WrapBody = Box<dyn FnOnce(InnerBody) -> Result<(), AnyError> + Send>;
pub(crate) type Wrapper<O, C> =
    Box<dyn FnOnce(&Arc<Spirit<O, C>>, InnerBody) -> Result<(), AnyError> + Send>;
pub(crate) type SpiritBody<O, C> =
    Box<dyn FnOnce(&Arc<Spirit<O, C>>) -> Result<(), AnyError> + Send>;
