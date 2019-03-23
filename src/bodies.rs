//! Utility body wrapper types.
//!
//! There are some problems with `Box<dyn FnOnce()>` types in Rust. The types here are less
//! convenient but actually usable types with similar function.

use std::sync::Arc;

use failure::Error;

use crate::spirit::Spirit;

pub(crate) trait Body<Param>: Send {
    fn run(&mut self, param: Param) -> Result<(), Error>;
}

impl<F: FnOnce(Param) -> Result<(), Error> + Send, Param> Body<Param> for Option<F> {
    fn run(&mut self, param: Param) -> Result<(), Error> {
        (self.take().expect("Body called multiple times"))(param)
    }
}

/// A workaround type for `Box<dyn FnOnce() -> Result<(), Error>`.
///
/// Since it is not possible to use the aforementioned type in any meaningful way in Rust yet, this
/// works around the problem. The type has a [`run`][InnerBody::run] method which does the same thing.
///
/// This is passed as parameter to the closure passed to
/// [`run_around`][crate::Extensible::run_around], representing the body to be run inside.
pub struct InnerBody(pub(crate) Box<dyn Body<()>>);

impl InnerBody {
    /// Run the body.
    pub fn run(mut self) -> Result<(), Error> {
        self.0.run(())
    }
}

/// A wrapper around a body.
pub(crate) struct WrapBody(pub(crate) Box<dyn Body<InnerBody>>);

impl WrapBody {
    /// Call the closure inside.
    pub(crate) fn run(mut self, inner: InnerBody) -> Result<(), Error> {
        self.0.run(inner)
    }
}

pub(crate) type Wrapper<O, C> = Box<dyn for<'a> Body<(&'a Arc<Spirit<O, C>>, InnerBody)>>;
pub(crate) type SpiritBody<O, C> = Box<dyn for<'a> Body<&'a Arc<Spirit<O, C>>>>;
