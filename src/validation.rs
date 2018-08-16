//! Helpers for configuration validation.
//!
//! See [`config_validator`](../struct.Builder.html#method.config_validator).
use std::fmt::{Debug, Formatter, Result as FmtResult};
use std::slice::Iter;

/// A level of [`validation result`](struct.Result.html).
///
/// This determines the log level at which the result (message) will appear. It also can determine
/// to refuse the whole configuration.
#[derive(Copy, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Level {
    /// No message is logged.
    ///
    /// This makes sense if you only want to set an action to be attached to a validation result.
    Nothing,

    /// A message printed on info level.
    ///
    /// This likely means nothing is wrong, but maybe something the user might be interested in ‒
    /// like simpler way to express it, the fact this will get deprecated in next version...
    Hint,

    /// A message printed on warning level.
    ///
    /// Something is wrong, but the application still can continue, like one of several replicas
    /// are not stored where they should, or the configuration contains only one replica so any
    /// failure would make losing data.
    Warning,

    /// A message printed on error level and refused configuration.
    ///
    /// In addition to printing the message on error level, the whole configuration is considered
    /// invalid and the application either doesn't start (if it is the first configuration) or
    /// keeps the old one.
    Error,
}

impl Default for Level {
    fn default() -> Self {
        Level::Nothing
    }
}

/// A validation result.
///
/// The validator (see [`config_validator`](../struct.Builder.html#method.config_validator)) is
/// supposed to return an arbitrary number of these results. Each one can hold a message (with
/// varying severity) and optionally a success and failure actions.
#[derive(Default)]
pub struct Result {
    level: Level,
    description: String,
    pub(crate) on_abort: Option<Box<FnMut() + Sync + Send>>,
    pub(crate) on_success: Option<Box<FnMut() + Sync + Send>>,
}

impl Result {
    /// Creates a result with given level and message.
    pub fn new<S: Into<String>>(level: Level, s: S) -> Self {
        Result {
            level,
            description: s.into(),
            .. Default::default()
        }
    }

    /// Creates a result without any message.
    pub fn nothing() -> Self {
        Self::new(Level::Nothing, "")
    }

    /// Creates a result with a hint.
    pub fn hint<S: Into<String>>(s: S) -> Self {
        Self::new(Level::Hint, s)
    }

    /// Creates a result with a warning.
    pub fn warning<S: Into<String>>(s: S) -> Self {
        Self::new(Level::Warning, s)
    }

    // TODO: Actual error something here?

    /// Creates an error result.
    ///
    /// An error result not only gets to the logs, it also marks the whole config as invalid.
    pub fn error<S: Into<String>>(s: S) -> Self {
        Self::new(Level::Error, s)
    }

    /// Returns the result's level.
    pub fn level(&self) -> Level {
        self.level
    }

    /// Returns the message texts.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Attaches (replaces) the success action.
    pub fn on_success<F: FnOnce() + Send + Sync + 'static>(self, f: F) -> Self {
        let mut f = Some(f);
        let wrapper = move || (f.take().unwrap())();
        Self {
            on_success: Some(Box::new(wrapper)),
            .. self
        }
    }

    /// Attaches (replaces) the failure action.
    pub fn on_abort<F: FnOnce() + Send + Sync + 'static>(self, f: F) -> Self {
        let mut f = Some(f);
        let wrapper = move || (f.take().unwrap())();
        Self {
            on_success: Some(Box::new(wrapper)),
            .. self
        }
    }
}

impl Debug for Result {
    fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
        fmt.debug_struct("ValidationResult")
            .field("level", &self.level)
            .field("description", &self.description)
            .field("on_abort", if self.on_abort.is_some() { &"Fn()" } else { &"None" })
            .field("on_success", if self.on_success.is_some() { &"Fn()" } else { &"None" })
            .finish()
    }
}

impl From<String> for Result {
    fn from(s: String) -> Self {
        Result {
            level: Level::Error,
            description: s,
            .. Default::default()
        }
    }
}

impl From<&'static str> for Result {
    fn from(s: &'static str) -> Self {
        Result {
            level: Level::Error,
            description: s.to_owned(),
            .. Default::default()
        }
    }
}

impl From<(Level, String)> for Result {
    fn from((level, s): (Level, String)) -> Self {
        Result {
            level,
            description: s,
            .. Default::default()
        }
    }
}

impl From<(Level, &'static str)> for Result {
    fn from((level, s): (Level, &'static str)) -> Self {
        Result {
            level,
            description: s.to_owned(),
            .. Default::default()
        }
    }
}

/// Multiple validation results.
///
/// A validator can actually return multiple results at once (to, for example, produce multiple
/// messages). This is a storage of multiple results which can be created by either converting a
/// single or iterator or by merging multiple `Results` objects.
#[derive(Debug, Default, Fail)]
#[fail(display = "Validation failed")] // TODO: Something better
pub struct Results(pub(crate) Vec<Result>);

impl Results {
    /// Creates an empty results storage.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends another bunch of results into this storage.
    pub fn merge<R: Into<Results>>(&mut self, other: R) {
        self.0.extend(other.into().0);
    }

    /// Iterates through the storage.
    pub fn iter(&self) -> Iter<Result> {
        self.into_iter()
    }

    /// The most severe level of all the results inside.
    pub fn max_level(&self) -> Option<Level> {
        self.iter().map(|r| r.level).max()
    }
}

impl<'a> IntoIterator for &'a Results {
    type Item = &'a Result;
    type IntoIter = Iter<'a, Result>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl From<Result> for Results {
    fn from(val: Result) -> Self {
        Results(vec![val])
    }
}

impl<I, It> From<I> for Results
where
    I: IntoIterator<Item = It>,
    It: Into<Result>,
{
    fn from(vals: I) -> Self {
        Results(vals.into_iter().map(Into::into).collect())
    }
}
