//! Deterministic rendering of the [`uncad_model`] entity model to SVG and
//! PNG.
//!
//! The same model and options produce the same bytes, every time. An entity
//! the renderer cannot draw faithfully is reported in
//! [`ToSvgResult::unsupported_types`], a block reference that contributed
//! nothing to the picture in [`ToSvgResult::empty_blocks`], one whose block
//! the model does not hold in [`ToSvgResult::unresolved_block_refs`], and
//! what the bounds on the file's numbers left out in
//! [`ToSvgResult::limits`] -- rather than dropped in silence -- and what the
//! drawing itself hides is counted in [`ToSvgResult::hidden`]. A paper
//! layout can be drawn as its sheet, with the model shown through its
//! viewports ([`layout_to_svg`], [`layout_to_png`]). A render is
//! for people and for an agent's fallback view; it is never evidence that a
//! drawing or an edit is correct.
//!
//! The rules are in `docs/principles.md`; what is approximated and what is
//! unverified is in `docs/CAVEATS.md`.

#![forbid(unsafe_code)]

pub mod color;
pub mod limits;
mod png;
mod svg;
mod text;

pub use png::{
    layout_to_png, svg_to_png, to_png, Background, Fonts, PngError, PngSize, ToPngOptions,
    ToPngResult, DEFAULT_MAX_EDGE,
};
pub use svg::{
    layout_to_svg, to_svg, LayoutError, Space, ToSvgOptions, ToSvgResult, DEFAULT_CAP_HEIGHT,
};
