//! What can go wrong, and how it reaches whoever needs to read it.
//!
//! Two audiences share one type. A tool failure is phrased for the model that called it, because
//! that message becomes the tool result and is what the agent acts on; everything else is phrased
//! for the person embedding the crate.

use std::fmt;

use crate::provider::ProviderFault;

/// An error the engine produced.
#[derive(Debug)]
pub enum RiffError {
    /// The agent bundle is not usable, so nothing may run against it.
    Bundle(String),
    /// A tool refused. The message is written for the model to act on.
    Tool(String),
    /// Something was asked of a session that has no connection.
    NotConnected,
    /// The embedding application could not answer.
    Host(HostError),
    /// The provider could not do it.
    Provider(ProviderFault),
}

/// What a [`crate::RiffHost`] or [`crate::RiffStore`] failed with.
///
/// Boxed rather than named so an embedder can return its own error type with `?` instead of
/// flattening it into a string the moment it crosses into Riff.
pub type HostError = Box<dyn std::error::Error + Send + Sync>;

/// The engine's result type.
pub type Result<T> = std::result::Result<T, RiffError>;

impl RiffError {
    /// A tool refusal, phrased for the model.
    pub fn tool(message: impl Into<String>) -> Self {
        RiffError::Tool(message.into())
    }

    /// A bundle this build will not run.
    pub fn bundle(message: impl Into<String>) -> Self {
        RiffError::Bundle(message.into())
    }
}

impl fmt::Display for RiffError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RiffError::Bundle(reason) => {
                write!(formatter, "the agent bundle is not usable: {reason}")
            }
            RiffError::Tool(message) => formatter.write_str(message),
            RiffError::NotConnected => formatter.write_str("the session is not connected"),
            RiffError::Host(error) => write!(formatter, "{error}"),
            RiffError::Provider(fault) => write!(formatter, "{fault}"),
        }
    }
}

impl std::error::Error for RiffError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RiffError::Host(error) => Some(error.as_ref()),
            RiffError::Provider(fault) => Some(fault),
            _ => None,
        }
    }
}

impl From<HostError> for RiffError {
    fn from(error: HostError) -> Self {
        RiffError::Host(error)
    }
}

impl From<ProviderFault> for RiffError {
    fn from(fault: ProviderFault) -> Self {
        RiffError::Provider(fault)
    }
}

impl fmt::Display for ProviderFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ProviderFault {}
