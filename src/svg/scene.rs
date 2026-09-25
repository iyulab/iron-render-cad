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
use super::{
    infinite, resolve_stroke_widths, sheet, CropReport, Hidden, LayoutError, LeftOutReason,
    SheetSource, ToSvgOptions, ViewportReport,
};
use crate::limits::LimitReport;
use crate::png::{parse, Fonts, PngError};
use resvg::usvg;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use uncad_model::model::{EntityId, Point2D};
use uncad_model::CadDatabase;

/// An axis-aligned rectangle of the drawing, in drawing units with y up --
/// the world a render shows, or the paper of a layout's sheet.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
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
    /// Whether the picture leaves the entity out, and why -- see
    /// [`CropReport::left_out`]. A part set aside as an outlier
    /// ([`LeftOutReason::ScaleOutlier`], [`LeftOutReason::FarOutlier`]) is
    /// not in [`crate::to_svg`]'s document; one outside the view is, beyond
    /// what the viewBox shows.
    pub left_out: Option<LeftOutReason>,
    /// `true` for the part of a layout's sheet ([`Scene::layout`]) that is
    /// the model drawn through the viewport [`id`](Self::id): one such part
    /// per viewport that shows the model, after the sheet's own entities,
    /// its extent the viewport's frame on the paper (nothing it draws
    /// reaches past the frame). The VIEWPORT entity's own frame is a
    /// separate part, an ordinary one.
    pub through_viewport: bool,
}

/// Where one text of a [`Scene`] is: the box the renderer estimated for it
/// while walking, and the box its glyphs fill once a set of fonts lays them
/// out.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TextBox {
    /// Which text this is: the text entity's reference ID, after the IDs
    /// of the block references it is drawn inside, outermost first -- and,
    /// on a layout's sheet, for the model drawn through a viewport, after
    /// that VIEWPORT's ID. A text inside a block placed twice is two boxes;
    /// the first ID is always the [`Part`] that draws it. The document's
    /// `<text>` element carries the path as its `id`: `t` and the IDs'
    /// values joined by `.` (`t12.40`).
    pub path: Vec<EntityId>,
    /// What it shows: its string with the control codes read, as drawn --
    /// an MTEXT's lines joined by `\n`.
    pub text: String,
    /// The box the renderer estimated while walking, in world units: its
    /// anchor, and a box 0.6 em a character wide and a capital tall, hung
    /// as the text is justified and taken through its own axes and every
    /// enclosing placement -- what the extent counted (except a TOLERANCE
    /// frame's, which counts its insertion point only). `None` when the
    /// placement took it past what a number holds.
    pub estimate: Option<Rect>,
    /// The box of its glyph outlines as the fonts lay them out, in world
    /// units, through every enclosing placement (a turned text's box is the
    /// box of its turned outline box). The layout is computed in single
    /// precision, relative to the scene's origin. `None` when nothing was
    /// laid out: no face had any of its glyphs, or it shows only spaces.
    pub measured: Option<Rect>,
    /// How many of its glyphs the fonts did not have and drew as the
    /// face's missing-glyph shape.
    pub missing_glyphs: usize,
}

/// A text a walk drew, before any font laid it out.
pub(super) struct DrawnText {
    pub(super) path: Vec<EntityId>,
    pub(super) text: String,
    pub(super) estimate: Option<Box2D>,
}

/// The `id` attribute of the `<text>` a text with this path is drawn as:
/// `t`, then the IDs' values joined by `.`. Unique in a document, since a
/// model's reference IDs are.
pub(super) fn text_id(path: &[EntityId]) -> String {
    let mut id = String::from("t");
    for (i, e) in path.iter().enumerate() {
        if i > 0 {
            id.push('.');
        }
        let _ = write!(id, "{}", e.value());
    }
    id
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
    pub crop: CropReport,
    pub viewports: Vec<ViewportReport>,
    pub sheet: Option<SheetSource>,
    /// Every text drawn, in drawing order; see [`text_boxes`](Self::text_boxes).
    pub(super) texts: Vec<DrawnText>,
}

/// The document-frame viewBox `[x, y, width, height]` of `window` for a
/// render written about `origin`.
pub(super) fn doc_view_box(window: &Rect, origin: Point2D) -> [f64; 4] {
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
    /// kept but the outliers the crop set aside ([`Part::left_out`]), it is
    /// [`crate::to_svg`]'s document, byte for byte.
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

    /// Every text the scene drew, in drawing order -- the ones in parts
    /// the crop set aside included -- with the box the renderer estimated
    /// and the box its glyphs fill when laid out with `fonts`, the way
    /// [`Scene::png`] draws them: a label's real extent, which the estimate
    /// can miss by the width of a few characters in a face wider or
    /// narrower than 0.6 em a character (a Hangul syllable is about 0.9).
    ///
    /// The texts are laid out once, in one document; the text's `id`
    /// attribute ([`TextBox::path`]) is how each box is found.
    pub fn text_boxes(&self, fonts: &Fonts) -> Result<Vec<TextBox>, PngError> {
        let mut boxes: Vec<TextBox> = self
            .texts
            .iter()
            .map(|t| TextBox {
                path: t.path.clone(),
                text: t.text.clone(),
                estimate: t.estimate.map(Rect::from),
                measured: None,
                missing_glyphs: 0,
            })
            .collect();
        if boxes.is_empty() {
            return Ok(boxes);
        }
        let by_id: BTreeMap<String, usize> = boxes
            .iter()
            .enumerate()
            .map(|(i, b)| (text_id(&b.path), i))
            .collect();
        let document = self.assemble(self.doc_view_box, self.auto_stroke_width, |i| {
            self.body[i].contains("<text")
        });
        let tree = parse(&document, fonts)?;
        let mut found = Vec::new();
        collect_texts(tree.root(), &mut found);
        // The canvas is the viewBox moved to (0, 0), y down, one unit a
        // unit: a canvas point is the document point less the viewBox's
        // corner, and the document is written about the origin.
        let [vx, vy, _, _] = self.doc_view_box;
        for (id, canvas, missing) in found {
            let Some(&i) = by_id.get(&id) else { continue };
            let world = Rect::new(
                self.origin.x + vx + f64::from(canvas.left()),
                self.origin.y - (vy + f64::from(canvas.bottom())),
                self.origin.x + vx + f64::from(canvas.right()),
                self.origin.y - (vy + f64::from(canvas.top())),
            );
            let finite = [world.min_x, world.min_y, world.max_x, world.max_y]
                .iter()
                .all(|v| v.is_finite());
            if finite {
                boxes[i].measured = Some(world);
                boxes[i].missing_glyphs = missing;
            }
        }
        Ok(boxes)
    }

    /// The document of every part the crop did not set aside, at
    /// `stroke_width`: what [`crate::to_svg`] writes.
    pub(crate) fn document(&self, stroke_width: f64) -> String {
        self.assemble(self.doc_view_box, stroke_width, |i| self.in_document(i))
    }

    /// [`view_box`](Self::view_box) as the document writes it.
    pub(crate) fn doc_view_box(&self) -> [f64; 4] {
        self.doc_view_box
    }

    /// The document with viewBox `[x, y, width, height]` (document frame)
    /// holding the parts `keep` accepts by index.
    fn assemble(
        &self,
        view_box: [f64; 4],
        stroke_width: f64,
        keep: impl Fn(usize) -> bool,
    ) -> String {
        let [x, y, width, height] = view_box;
        let defs_block = self.defs_block(stroke_width);
        let resolved_body = self.layer(view_box, stroke_width, keep);
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{x} {y} {width} {height}\" stroke=\"black\" stroke-width=\"{stroke_width}\">\n  {defs_block}{resolved_body}\n</svg>"
        )
    }

    /// The elements of the parts `keep` accepts by index, in drawing order,
    /// for a document with viewBox `[x, y, width, height]` (document frame)
    /// at `stroke_width`: stroke widths resolved and construction lines cut
    /// to that window. What a document holds between its `<defs>` and its
    /// end.
    pub(super) fn layer(
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
        infinite::resolve(
            resolve_stroke_widths(&body.join("\n  "), stroke_width),
            infinite::window(x, y, width, height, stroke_width),
        )
    }

    /// The `<defs>` block and the indent after it, or nothing when the
    /// scene defines nothing. HATCH pattern defs carry stroke-width
    /// placeholders too; an empty defs list emits no block at all.
    pub(super) fn defs_block(&self, stroke_width: f64) -> String {
        if self.defs.is_empty() {
            String::new()
        } else {
            let resolved_defs = resolve_stroke_widths(&self.defs.join("\n  "), stroke_width);
            format!("<defs>\n  {resolved_defs}\n</defs>\n  ")
        }
    }

    /// Whether [`crate::to_svg`]'s document holds part `i`: every part but
    /// the outliers the crop set aside.
    pub(super) fn in_document(&self, i: usize) -> bool {
        !matches!(
            self.parts[i].left_out,
            Some(LeftOutReason::ScaleOutlier | LeftOutReason::FarOutlier)
        )
    }
}

/// Every text node under `group` that has an id and laid out at least one
/// glyph: its id, the box of its glyph outlines on the canvas, and how many
/// of its glyphs are the missing-glyph shape.
fn collect_texts(group: &usvg::Group, out: &mut Vec<(String, usvg::Rect, usize)>) {
    for node in group.children() {
        match node {
            usvg::Node::Group(g) => collect_texts(g, out),
            usvg::Node::Text(t) if !t.id().is_empty() => {
                let glyphs = t.layouted().iter().flat_map(|span| &span.positioned_glyphs);
                let (count, missing) =
                    glyphs.fold((0, 0), |(n, m), g| (n + 1, m + usize::from(g.id.0 == 0)));
                if count > 0 {
                    out.push((t.id().to_string(), t.abs_stroke_bounding_box(), missing));
                }
            }
            _ => {}
        }
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
    fn a_texts_id_is_its_path() {
        let path = [EntityId::new(12), EntityId::new(40), EntityId::new(7)];
        assert_eq!(text_id(&path), "t12.40.7");
        assert_eq!(text_id(&path[..1]), "t12");
    }

    #[test]
    fn a_scene_can_be_shared_between_threads() {
        fn shareable<T: Send + Sync>() {}
        shareable::<Scene>();
    }
}
