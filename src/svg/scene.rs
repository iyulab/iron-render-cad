//! Render once, assemble many.
//!
//! [`crate::to_svg`] walks the drawing, measures it, frames it and writes
//! one document. A caller that wants several pictures of the same drawing
//! -- tiles of a large plan, a detail at another scale, the same sheet at
//! two sizes -- would otherwise walk the whole drawing again for each. A
//! [`Scene`] is that walk kept: every top-level entity drawn once, as a
//! [`Part`] that says which entity it is and what box of the world it
//! covers, with the document-wide pieces (pattern definitions, the
//! placeholders for stroke widths and construction lines) still open.
//! [`Scene::svg`] then writes a document for any window of the world, from
//! any subset of the parts, at any stroke width.

use super::bounds::Box2D;
use super::{infinite, resolve_stroke_widths, sheet, Hidden, LayoutError, ToSvgOptions};
use crate::limits::LimitReport;
use uncad_model::model::{EntityId, Point2D};
use uncad_model::CadDatabase;

/// An axis-aligned rectangle of the drawing, in drawing units with y up --
/// the world a render shows, or the paper of a layout's sheet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl Rect {
    pub const fn new(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Rect {
        Rect {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }

    pub fn width(&self) -> f64 {
        self.max_x - self.min_x
    }

    pub fn height(&self) -> f64 {
        self.max_y - self.min_y
    }

    /// Whether the two share a point: overlapping, or touching at an edge
    /// or a corner.
    pub fn intersects(&self, other: &Rect) -> bool {
        self.min_x <= other.max_x
            && other.min_x <= self.max_x
            && self.min_y <= other.max_y
            && other.min_y <= self.max_y
    }
}

impl From<Box2D> for Rect {
    fn from(b: Box2D) -> Rect {
        Rect::new(b.min_x, b.min_y, b.max_x, b.max_y)
    }
}

impl From<Rect> for Box2D {
    fn from(r: Rect) -> Box2D {
        Box2D {
            min_x: r.min_x,
            max_x: r.max_x,
            min_y: r.min_y,
            max_y: r.max_y,
        }
    }
}

/// What one top-level entity of a [`Scene`] drew.
///
/// Every entity the render walked has a part, in drawing order, whether it
/// drew anything or not: one the drawing hides, one this renderer has
/// nothing to draw for, one a bound in [`crate::limits`] left out.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Part {
    /// The entity's reference ID. For a part that is the model seen
    /// through a viewport (see [`through_viewport`](Self::through_viewport)),
    /// the VIEWPORT's.
    pub id: EntityId,
    /// Its DXF type name.
    pub type_name: String,
    /// The box of the world it covers, as the render measured it: its
    /// geometry, a text by its estimated box, a block reference by
    /// everything it places; strokes are not counted. `None` when it
    /// measured nothing -- it drew nothing, the drawing hides it (a hidden
    /// entity never counts towards an extent, drawn faded or not), or a
    /// bound left it out.
    pub extent: Option<Rect>,
    /// Whether it wrote anything into the picture.
    pub drawn: bool,
    /// Whether it draws a construction line (a RAY or an XLINE, or a block
    /// reference holding one). Such a line counts only its base point
    /// towards the extent, but it reaches every window it crosses: a caller
    /// that keeps parts by their extent keeps this one whatever its extent
    /// says, and [`Scene::svg`] cuts the line to the window it writes.
    pub unbounded: bool,
    /// Why the drawing hides the entity, when it does. A hidden entity is
    /// drawn only with [`ToSvgOptions::include_hidden`] (faded), and is
    /// counted in [`Scene::hidden`] either way.
    pub hidden: Option<Hidden>,
    /// `true` for the part of a layout's sheet ([`Scene::layout`]) that is
    /// the model drawn through the viewport [`id`](Self::id): one such part
    /// per viewport that shows the model, after the sheet's own entities,
    /// its extent the viewport's frame on the paper (nothing it draws
    /// reaches past the frame). The VIEWPORT entity's own frame is a
    /// separate part, an ordinary one.
    pub through_viewport: bool,
}

/// A render kept for assembling: the drawing walked once, its top-level
/// entities as [`Part`]s, from which [`svg`](Self::svg) writes a document
/// for any window of the world and any subset of the parts.
///
/// The public fields are what [`crate::to_svg`] reports for the same
/// drawing and options, and mean the same; [`crate::ToSvgResult`] documents
/// them. A `Scene` is `Send` and `Sync`, so the documents of one scene can
/// be written on several threads.
pub struct Scene {
    pub(super) parts: Vec<Part>,
    /// Each part's elements, parallel to `parts`: empty for a part that
    /// drew nothing. Stroke widths and construction lines are still
    /// placeholders here.
    pub(super) body: Vec<String>,
    /// `<defs>` entries (HATCH patterns, a sheet's viewport clips), shared
    /// by every part that refers to them.
    pub(super) defs: Vec<String>,
    /// [`view_box`](Self::view_box) as the document writes it: `x, y,
    /// width, height` relative to [`origin`](Self::origin), y down.
    pub(super) doc_view_box: [f64; 4],
    /// The world rectangle the render frames: the extent (trimmed as the
    /// options say), padded. What [`crate::to_svg`]'s document shows.
    pub view_box: Rect,
    /// The world point the document's coordinates are written relative to;
    /// see [`crate::ToSvgResult::origin`]. Every [`Rect`] of a scene --
    /// the view box, the parts' extents, a window -- is in world units;
    /// this only says how they are written.
    pub origin: Point2D,
    /// The stroke width, in drawing units, [`crate::to_svg`] uses when none
    /// is given: about 1/6000th of the view box's diagonal.
    pub auto_stroke_width: f64,
    pub unsupported_types: Vec<String>,
    pub empty_blocks: Vec<String>,
    pub unresolved_block_refs: Vec<EntityId>,
    pub limits: LimitReport,
    pub hidden: usize,
    pub undrawn_viewports: Vec<EntityId>,
}

/// The document-frame viewBox `[x, y, width, height]` of `window` for a
/// render written about `origin`.
fn doc_view_box(window: &Rect, origin: Point2D) -> [f64; 4] {
    [
        window.min_x - origin.x,
        origin.y - window.max_y,
        window.width(),
        window.height(),
    ]
}

/// The world rectangle of a document-frame viewBox.
pub(super) fn world_rect([x, y, width, height]: [f64; 4], origin: Point2D) -> Rect {
    Rect::new(
        origin.x + x,
        origin.y - (y + height),
        origin.x + x + width,
        origin.y - y,
    )
}

impl Scene {
    /// Renders every entity of `options.space`, as [`crate::to_svg`] does,
    /// and keeps the render.
    pub fn new(db: &CadDatabase, options: ToSvgOptions) -> Scene {
        super::render(db, options)
    }

    /// Renders the paper layout named `layout` as its sheet, as
    /// [`crate::layout_to_svg`] does, and keeps the render. Its world is the
    /// paper, in the layout's paper units.
    pub fn layout(
        db: &CadDatabase,
        layout: &str,
        options: ToSvgOptions,
    ) -> Result<Scene, LayoutError> {
        sheet::render_layout(db, layout, options)
    }

    /// The parts: one per top-level entity walked, in drawing order, then
    /// -- for a layout's sheet -- one per viewport that shows the model.
    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// The SVG document showing the world rectangle `window`, drawn from the
    /// parts `keep` accepts, in drawing order, with every stroke
    /// `stroke_width` drawing units wide (thinner inside a scaled block
    /// reference, so every stroke looks the same) and every construction
    /// line cut to the window.
    ///
    /// The document is written relative to [`origin`](Self::origin) like
    /// [`crate::to_svg`]'s, so its `viewBox` is `window` moved by it:
    /// `(window.min_x - origin.x, origin.y - window.max_y, width, height)`.
    /// With `window` the scene's [`view_box`](Self::view_box), the stroke
    /// width [`auto_stroke_width`](Self::auto_stroke_width) and every part
    /// kept, it is [`crate::to_svg`]'s document, byte for byte.
    pub fn svg(&self, window: Rect, stroke_width: f64, keep: impl Fn(&Part) -> bool) -> String {
        // The scene's own view box is written exactly as the render framed
        // it: going through world units and back can move its last bit.
        let view_box = if window == self.view_box {
            self.doc_view_box
        } else {
            doc_view_box(&window, self.origin)
        };
        self.assemble(view_box, stroke_width, |i| keep(&self.parts[i]))
    }

    /// The document of every part at `stroke_width`: what
    /// [`crate::to_svg`] writes.
    pub(crate) fn document(&self, stroke_width: f64) -> String {
        self.assemble(self.doc_view_box, stroke_width, |_| true)
    }

    /// [`view_box`](Self::view_box) as the document writes it.
    pub(crate) fn doc_view_box(&self) -> [f64; 4] {
        self.doc_view_box
    }

    /// The document with viewBox `[x, y, width, height]` (document frame)
    /// holding the parts `keep` accepts by index.
    fn assemble(
        &self,
        [x, y, width, height]: [f64; 4],
        stroke_width: f64,
        keep: impl Fn(usize) -> bool,
    ) -> String {
        let body: Vec<&str> = self
            .body
            .iter()
            .enumerate()
            .filter(|(i, svg)| !svg.is_empty() && keep(*i))
            .map(|(_, svg)| svg.as_str())
            .collect();
        let resolved_body = infinite::resolve(
            resolve_stroke_widths(&body.join("\n  "), stroke_width),
            infinite::window(x, y, width, height, stroke_width),
        );
        // HATCH pattern defs carry stroke-width placeholders too. Kept
        // separate from the body only so an empty defs list emits no
        // <defs> block at all.
        let defs_block = if self.defs.is_empty() {
            String::new()
        } else {
            let resolved_defs = resolve_stroke_widths(&self.defs.join("\n  "), stroke_width);
            format!("<defs>\n  {resolved_defs}\n</defs>\n  ")
        };
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{x} {y} {width} {height}\" stroke=\"black\" stroke-width=\"{stroke_width}\">\n  {defs_block}{resolved_body}\n</svg>"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_and_its_document_view_box_convert_both_ways() {
        let origin = Point2D {
            x: 100_000.0,
            y: -50_000.0,
        };
        let window = Rect::new(99_990.0, -50_020.0, 100_030.0, -49_990.0);
        let vb = doc_view_box(&window, origin);
        assert_eq!(vb, [-10.0, -10.0, 40.0, 30.0]);
        assert_eq!(world_rect(vb, origin), window);
    }

    #[test]
    fn rectangles_intersect_when_they_share_a_point() {
        let a = Rect::new(0.0, 0.0, 1.0, 1.0);
        assert!(a.intersects(&Rect::new(1.0, 1.0, 2.0, 2.0)));
        assert!(!a.intersects(&Rect::new(1.5, 0.0, 2.0, 1.0)));
        assert_eq!((a.width(), a.height()), (1.0, 1.0));
    }

    #[test]
    fn a_scene_can_be_shared_between_threads() {
        fn shareable<T: Send + Sync>() {}
        shareable::<Scene>();
    }
}
