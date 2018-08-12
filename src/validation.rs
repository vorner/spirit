use std::fmt::{Debug, Formatter, Result as FmtResult};
use std::slice::Iter;

#[derive(Copy, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Level {
    Nothing,
    Hint,
    Warning,
    Error,
}

impl Default for Level {
    fn default() -> Self {
        Level::Nothing
    }
}

#[derive(Default)]
pub struct Result {
    level: Level,
    description: String,
    pub(crate) on_abort: Option<Box<FnMut() + Sync + Send>>,
    pub(crate) on_success: Option<Box<FnMut() + Sync + Send>>,
}

impl Result {
    pub fn new<S: Into<String>>(level: Level, s: S) -> Self {
        Result {
            level,
            description: s.into(),
            .. Default::default()
        }
    }
    pub fn nothing() -> Self {
        Self::new(Level::Nothing, "")
    }
    pub fn hint<S: Into<String>>(s: S) -> Self {
        Self::new(Level::Hint, s)
    }
    pub fn warning<S: Into<String>>(s: S) -> Self {
        Self::new(Level::Warning, s)
    }
    pub fn error<S: Into<String>>(s: S) -> Self {
        Self::new(Level::Error, s)
    }
    // TODO: Actual error something here?
    pub fn level(&self) -> Level {
        self.level
    }
    pub fn description(&self) -> &str {
        &self.description
    }
    pub fn on_success<F: FnOnce() + Send + Sync + 'static>(self, f: F) -> Self {
        let mut f = Some(f);
        let wrapper = move || (f.take().unwrap())();
        Self {
            on_success: Some(Box::new(wrapper)),
            .. self
        }
    }
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

#[derive(Debug, Default, Fail)]
#[fail(display = "Validation failed")] // TODO: Something better
pub struct Results(pub(crate) Vec<Result>);

impl Results {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn merge<R: Into<Results>>(&mut self, other: R) {
        self.0.extend(other.into().0);
    }
    pub fn iter(&self) -> Iter<Result> {
        self.into_iter()
    }
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

