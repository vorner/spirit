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

/// A workaround type for `Box<FnOnce() -> Result<(), Error>`.
///
/// Since it is not possible to use the aforementioned type in any meaningful way in Rust yet, this
/// works around the problem. The type has a [`run`](#method.run) method which does the same thing.
///
/// This is passed as parameter to the [`body_wrapper`](struct.Builder.html#method.body_wrapper).
///
/// It is also returned as part of the [`build`](struct.Builder.html#method.build)'s result.
pub struct InnerBody(pub(crate) Box<Body<()>>);

impl InnerBody {
    /// Run the body.
    pub fn run(mut self) -> Result<(), Error> {
        self.0.run(())
    }
}

/// A wrapper around a body.
///
/// These are essentially boxed closures submitted by
/// [`body_wrapper`](struct.Builder.html#method.body_wrapper) (or all of them folded together), but
/// in a form that is usable (in contrast to `Box<FnOnce(InnerBody) -> Result(), Error>`). It
/// is part of the return value of [`build`](struct.Builder.html#method.build) and the caller
/// should call it eventually.
pub struct WrapBody(pub(crate) Box<Body<InnerBody>>);

impl WrapBody {
    /// Call the closure inside.
    pub fn run(mut self, inner: InnerBody) -> Result<(), Error> {
        self.0.run(inner)
    }
}

pub(crate) type Wrapper<O, C> = Box<for<'a> Body<(&'a Arc<Spirit<O, C>>, InnerBody)>>;
pub(crate) type SpiritBody<O, C> = Box<for<'a> Body<&'a Arc<Spirit<O, C>>>>;
