//! Deterministic rendering of the [`uncad_model`] entity model to SVG and
//! PNG.
//!
//! The same model and options produce the same bytes, every time. An entity
//! the renderer cannot draw faithfully is reported in
//! [`ToSvgResult::unsupported_types`], and a block reference that
//! contributed nothing to the picture in [`ToSvgResult::empty_blocks`],
//! rather than dropped in silence. A render is for people and for an agent's
//! fallback view; it is never evidence that a drawing or an edit is correct.
//!
//! The rules are in `docs/principles.md`; what is approximated and what is
//! unverified is in `docs/CAVEATS.md`.

#![forbid(unsafe_code)]

pub mod color;
pub mod limits;
mod png;
mod svg;
mod text;

pub use png::{svg_to_png, to_png, PngError, ToPngOptions, ToPngResult, DEFAULT_MAX_EDGE};
pub use svg::{to_svg, Space, ToSvgOptions, ToSvgResult};
