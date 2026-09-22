use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Runtime process mode selected for `trellis-server`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeMode {
    /// Run every runtime subsystem in one process.
    All,
    /// Run only the platform subsystem.
    Platform,
    /// Run only the jobs subsystem.
    Jobs,
    /// Run only the health subsystem.
    Health,
    /// Run only the Events subsystem.
    Events,
}

impl RuntimeMode {
    /// Returns the subsystem set owned by this runtime process mode.
    #[must_use]
    pub fn subsystems(self) -> &'static [SubsystemName] {
        match self {
            Self::All => &[
                SubsystemName::Platform,
                SubsystemName::Jobs,
                SubsystemName::Health,
                SubsystemName::Events,
            ],
            Self::Platform => &[SubsystemName::Platform],
            Self::Jobs => &[SubsystemName::Jobs],
            Self::Health => &[SubsystemName::Health],
            Self::Events => &[SubsystemName::Events],
        }
    }
}

impl fmt::Display for RuntimeMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::All => "all",
            Self::Platform => "platform",
            Self::Jobs => "jobs",
            Self::Health => "health",
            Self::Events => "events",
        })
    }
}

impl FromStr for RuntimeMode {
    type Err = RuntimeModeParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "all" => Ok(Self::All),
            "platform" => Ok(Self::Platform),
            "jobs" => Ok(Self::Jobs),
            "health" => Ok(Self::Health),
            "events" => Ok(Self::Events),
            _ => Err(RuntimeModeParseError {
                value: value.to_owned(),
            }),
        }
    }
}

/// Runtime subsystem selected by a process mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubsystemName {
    /// Platform subsystem.
    Platform,
    /// Jobs subsystem.
    Jobs,
    /// Health subsystem.
    Health,
    /// Events subsystem.
    Events,
}

impl SubsystemName {
    /// Returns the canonical configuration section name for the subsystem.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Jobs => "jobs",
            Self::Health => "health",
            Self::Events => "events",
        }
    }
}

impl fmt::Display for SubsystemName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Error returned when parsing a runtime process mode fails.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("unsupported runtime mode {value:?}; expected one of: all, platform, jobs, health, events")]
pub struct RuntimeModeParseError {
    /// Original unsupported mode value.
    pub value: String,
}
