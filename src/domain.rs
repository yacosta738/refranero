//! Shared domain concepts, independent of persistence and HTTP.
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Supported paremiological categories.
#[derive(Debug, Copy, Clone, Eq, PartialEq, ValueEnum)]
pub enum Category {
    Refran,
    Paremia,
    Proverbio,
    Dicho,
    Adivinanza,
    Cita,
}

impl Category {
    /// Stable name used by persistence and CLI output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Refran => "refran",
            Self::Paremia => "paremia",
            Self::Proverbio => "proverbio",
            Self::Dicho => "dicho",
            Self::Adivinanza => "adivinanza",
            Self::Cita => "cita",
        }
    }

    /// Stable URL segment in Biblioteca Virtual Miguel de Cervantes.
    pub const fn url_segment(self) -> &'static str {
        match self {
            Self::Refran => "refran",
            Self::Paremia => "refran",
            Self::Proverbio => "proverbio",
            Self::Dicho => "dicho",
            Self::Adivinanza => "adivinanza",
            Self::Cita => "cita_literaria",
        }
    }

    /// All categories for an explicit broad synchronization.
    pub const fn all() -> [Self; 5] {
        [
            Self::Refran,
            Self::Proverbio,
            Self::Dicho,
            Self::Adivinanza,
            Self::Cita,
        ]
    }
}

/// New saying before it acquires a local database ID.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NewSaying {
    pub text: String,
    pub category: String,
    pub language: String,
    pub source: String,
    pub source_url: String,
    #[serde(default)]
    pub original_url: Option<String>,
    #[serde(default)]
    pub captured_at: Option<String>,
    #[serde(default)]
    pub group_id: Option<String>,
}

/// Persisted saying; a group ID can later connect multilingual equivalents.
#[derive(Debug, Clone, Serialize)]
pub struct Saying {
    pub id: i64,
    #[serde(flatten)]
    pub entry: NewSaying,
}
