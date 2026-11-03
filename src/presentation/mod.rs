//! Terminal presentation layer.
//!
//! Composition is pure: [`render`] receives already-resolved [`RenderOptions`] and never reads the
//! environment, the network or the catalog. Terminal capabilities are resolved once in
//! [`detect`], so layouts stay deterministic and snapshot-testable.

mod label;
mod renderer;
mod sanitize;
mod screen;
mod terminal;
mod theme;

pub use label::{category as category_name, language as language_name};
pub use renderer::render;
pub use screen::{compose, ProgressView, Screen};
pub use terminal::{detect, ColorMode};

/// Rendering context resolved from the terminal, the environment and CLI flags.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct RenderOptions {
    /// Available visual columns.
    pub width: usize,
    /// Emit ANSI styles.
    pub color: bool,
    /// Emit OSC 8 hyperlinks.
    pub links: bool,
    /// Decorate the output; plain output stays machine-friendly.
    pub interactive: bool,
}

impl RenderOptions {
    /// Unstyled options for redirected output and tests.
    pub const fn plain(width: usize) -> Self {
        Self {
            width,
            color: false,
            links: false,
            interactive: false,
        }
    }
}
