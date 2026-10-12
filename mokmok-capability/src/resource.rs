use alloc::string::String;
use core::fmt;

use crate::name::Name;

/// What an operation acts on.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    Path(String),
    Tool(Name),
    Model(Name),
    Host { host: String, port: u16 },
}

impl fmt::Display for Resource {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::Path(path) => write!(formatter, "path {path}"),
            Self::Tool(name) => write!(formatter, "tool {name}"),
            Self::Model(name) => write!(formatter, "model {name}"),
            Self::Host { host, port } => write!(formatter, "host {host}:{port}"),
        }
    }
}
