//! Helpers for configuration validation.
//!
//! See [`config_validator`][crate::Extensible::config_validator].

/// A validation action.
///
/// The validator (see [`config_validator`][crate::Extensible::config_validator]) is
/// supposed to either return an error or an action to be taken once validation completes.
///
/// By default, the [`Action`] is empty, but an [`on_success`][Action::on_success] and
/// [`on_abort`][Action::on_abort] callbacks can be attached to it. These'll execute once the
/// validation completes (only one of them will be called, depending on the result of validation).
///
/// # Examples
///
/// ```rust
/// use spirit::{Empty, Spirit};
/// use spirit::prelude::*;
/// use spirit::validation::Action;
/// # fn create_something<T>(_cfg: T) -> Result<Empty, failure::Error> { Ok(Empty {}) }
/// # fn install_something(_empty: Empty) {}
/// # let _ =
/// Spirit::<Empty, Empty>::new()
///     .config_validator(|_old_cfg, new_cfg, _opts| {
///         let something = create_something(new_cfg)?;
///         Ok(Action::new().on_success(move || install_something(something)))
///     });
/// ```
///
/// Or, if you want to only check the configuration:
///
/// ```rust
/// use failure::ensure;
/// use spirit::{Empty, Spirit};
/// use spirit::prelude::*;
/// use spirit::validation::Action;
/// # fn looks_good<T>(_cfg: T) -> bool { true }
/// # let _ =
/// Spirit::<Empty, Empty>::new()
///     .config_validator(|_old_cfg, new_cfg, _opts| {
///         ensure!(looks_good(new_cfg), "Configuration is broken");
///         Ok(Action::new())
///     });
/// ```
#[derive(Default)]
pub struct Action {
    pub(crate) on_abort: Option<Box<dyn FnMut()>>,
    pub(crate) on_success: Option<Box<dyn FnMut()>>,
}

impl Action {
    /// Creates actions wit both hooks empty.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches (replaces) the success action.
    pub fn on_success<F: FnOnce() + 'static>(self, f: F) -> Self {
        let mut f = Some(f);
        let wrapper = move || (f.take().unwrap())();
        Self {
            on_success: Some(Box::new(wrapper)),
            ..self
        }
    }

    /// Attaches (replaces) the failure action.
    pub fn on_abort<F: FnOnce() + 'static>(self, f: F) -> Self {
        let mut f = Some(f);
        let wrapper = move || (f.take().unwrap())();
        Self {
            on_abort: Some(Box::new(wrapper)),
            ..self
        }
    }

    pub(crate) fn run(self, success: bool) {
        let selected = if success {
            self.on_success
        } else {
            self.on_abort
        };
        if let Some(mut cback) = selected {
            cback();
        }
    }
}
