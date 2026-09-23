//! Single-line text -- TEXT and ATTRIB -- placed the way the file justifies
//! it: which point it hangs from and how (DXF 72/73/74 with the alignment
//! point, DXF 11), how wide its characters are (41) and how far they slant
//! (51), and the box it is estimated to fill, which is what it counts
//! towards the picture's extent.
//!
//! The renderer has no font metrics while it walks the drawing -- the face
//! is only chosen when a PNG is rasterized -- so where a rule needs one it
//! states an assumption, in ems of the face the text is drawn with:
//! [`DESCENDER_EM`] and [`CHAR_ADVANCE_EM`]. Both are turned into text
//! heights through [`crate::ToSvgOptions::cap_height`], the one metric a
//! caller states.

use super::bounds::Box2D;
use super::format::{clean, escape_xml, neg, rotate_transform_attr, Frame};
use super::Ctx;
use std::fmt::Write as _;
use uncad_model::model::{
    HorizontalJustification, MTextAttachment, Point2D, VerticalJustification,
};
use uncad_model::Ocs;

/// How far a text's descenders reach below its baseline, in ems: 0.2, a
/// Latin `p` or `g` in the common sans-serif faces. A text justified to its
/// *bottom* stands on its descenders, so its baseline is this much above
/// the alignment point.
pub(super) const DESCENDER_EM: f64 = 0.2;

/// The advance a character is assumed to take, in ems: 0.6, a typical
/// average for a sans-serif face. What a text's box is estimated from; the
/// glyphs themselves are laid out by the rasterizer.
pub(super) const CHAR_ADVANCE_EM: f64 = 0.6;

/// Where a single-line text is drawn from, in the entity's own coordinates,
/// and how it hangs off that point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Anchor {
    /// The point the text is justified at.
    pub(super) at: Point2D,
    /// The SVG `text-anchor`: which end of the baseline is at `at`.
    pub(super) anchor: &'static str,
    /// How far the baseline is below `at`, in text heights, along the
    /// text's own upright direction: 0 on the baseline, 1 for a text hung
    /// from the top of its capitals.
    pub(super) drop: f64,
}

/// The file's justification, as SVG can draw it. A left/baseline text is
/// placed by its start point alone; every other one by its alignment point,
/// which the model states for exactly those. Horizontally, centre and
/// middle centre on it, right ends on it, and aligned and fit -- which fill
/// the baseline from the start point to the alignment point by stretching
/// to a width only the font's metrics give -- are centred between the two.
/// Vertically, the text height is the height of the capitals, so a
/// top-justified text's baseline is one height below the point and a
/// middle-justified one's half a height; a bottom-justified text stands on
/// its descenders, `descender` text heights below the baseline. MIDDLE is
/// centred both ways whatever its vertical justification says.
///
/// A justification whose file states no alignment point falls back to the
/// start point, left and on the baseline: nothing is guessed.
pub(super) fn anchor(
    start: Point2D,
    alignment: Option<Point2D>,
    horizontal: HorizontalJustification,
    vertical: VerticalJustification,
    descender: f64,
) -> Anchor {
    use HorizontalJustification as H;
    use VerticalJustification as V;
    let Some(align) = alignment else {
        return Anchor {
            at: start,
            anchor: "start",
            drop: 0.0,
        };
    };
    let (at, anchor) = match horizontal {
        H::Left => (align, "start"),
        H::Center | H::Middle => (align, "middle"),
        H::Right => (align, "end"),
        H::Aligned | H::Fit => (
            Point2D {
                x: (start.x + align.x) / 2.0,
                y: (start.y + align.y) / 2.0,
            },
            "middle",
        ),
    };
    let drop = match (horizontal, vertical) {
        (H::Middle, _) | (_, V::Middle) => 0.5,
        (_, V::Top) => 1.0,
        (_, V::Bottom) => -descender,
        (_, V::Baseline) => 0.0,
    };
    Anchor { at, anchor, drop }
}

/// How a text's own axes -- along its baseline, and up its characters --
/// lie in the entity's coordinates: turned by `rotation`, the characters
/// slanted by `oblique` (both radians) and stretched by `width_factor`
/// along the baseline. As `[a, b, c, d]`, the map `(u, v) -> (a u + c v,
/// b u + d v)`, y up.
pub(super) fn text_axes(rotation: f64, oblique: f64, width_factor: f64) -> [f64; 4] {
    let (sin, cos) = rotation.sin_cos();
    let slant = oblique.tan();
    [
        width_factor * cos,
        width_factor * sin,
        slant * cos - sin,
        slant * sin + cos,
    ]
}

/// The width factor a text is drawn at: the stored one, or 1 when the file
/// stores 0 or something that is not a number. A zero would flatten the
/// text to nothing; the reference's default is 1.
pub(super) fn effective_width_factor(stored: f64) -> f64 {
    if stored.is_finite() && stored != 0.0 {
        stored
    } else {
        1.0
    }
}

/// Everything about a single-line `<text>` but its string and colour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct TextLayout {
    pub(super) anchor: Anchor,
    /// The CAD height, already made positive (see
    /// [`super::effective_text_height`]).
    pub(super) height: f64,
    /// The rotation, radians, when `axes` is that rotation and nothing
    /// else: the text is then written with a plain `rotate()`, as it always
    /// was. `None` when the characters are stretched or slanted.
    pub(super) rotation: Option<f64>,
    /// See [`text_axes`].
    pub(super) axes: [f64; 4],
}

impl TextLayout {
    pub(super) fn new(
        anchor: Anchor,
        height: f64,
        rotation: f64,
        oblique: f64,
        width_factor: f64,
    ) -> TextLayout {
        let width_factor = effective_width_factor(width_factor);
        let plain = width_factor == 1.0 && oblique == 0.0;
        TextLayout {
            anchor,
            height,
            rotation: plain.then_some(rotation),
            axes: text_axes(rotation, oblique, width_factor),
        }
    }

    /// This layout, stated at height `z` in `plane`, as it lies in the world
    /// seen from above: the anchor taken there, and the text's axes with it
    /// -- so a mirrored text reads backwards, as a text seen from behind
    /// does, and one on a tilted plane is foreshortened.
    pub(super) fn in_plane(self, plane: Ocs, z: f64) -> TextLayout {
        if plane.is_world() {
            return self;
        }
        let m = super::plane_seen_from_above(plane, z);
        let [a, b, c, d] = self.axes;
        TextLayout {
            anchor: Anchor {
                at: m.apply(self.anchor.at),
                ..self.anchor
            },
            height: self.height,
            rotation: None,
            axes: [
                m.a * a + m.c * b,
                m.b * a + m.d * b,
                m.a * c + m.c * d,
                m.b * c + m.d * d,
            ],
        }
    }
}

/// A single-line `<text>` -- TEXT, ATTRIB and TOLERANCE all render to this.
/// `id` is its `id` attribute (see [`super::scene::text_id`]); `text` is
/// what is shown, already decoded; the CAD height is the height of the
/// capitals, written as the `font-size` that makes the capitals that tall
/// in a face whose capitals are `cap_height` of the em.
///
/// A text that is only turned is written at its baseline's point with a
/// `rotate()` about its anchor. One whose characters are stretched or
/// slanted is written at the origin of a `matrix()` that carries the text's
/// own axes: SVG lays the glyphs out -- `text-anchor` included -- in the
/// element's coordinates, and the matrix then stretches and slants them.
pub(super) fn text_element(
    id: &str,
    layout: &TextLayout,
    color: &str,
    text: &str,
    frame: Frame,
    cap_height: f64,
) -> String {
    let font_size = layout.height / cap_height;
    let (x, y) = (frame.x(layout.anchor.at.x), frame.y(layout.anchor.at.y));
    let baseline = clean(layout.anchor.drop * layout.height);
    let anchor_attr = match layout.anchor.anchor {
        "start" => String::new(),
        other => format!(" text-anchor=\"{other}\""),
    };
    let mut out = String::new();
    match layout.rotation {
        Some(rotation) => {
            let _ = write!(
                out,
                "<text id=\"{id}\" x=\"{x}\" y=\"{}\" font-size=\"{font_size}\" fill=\"{color}\" stroke=\"none\"{anchor_attr}{}>",
                clean(y + baseline),
                rotate_transform_attr(rotation, x, y),
            );
        }
        None => {
            // The axes with y flipped on both sides, like a block's matrix.
            let [a, b, c, d] = layout.axes;
            let _ = write!(
                out,
                "<text id=\"{id}\" x=\"0\" y=\"{baseline}\" font-size=\"{font_size}\" fill=\"{color}\" stroke=\"none\"{anchor_attr} transform=\"matrix({} {} {} {} {x} {y})\">",
                clean(a),
                neg(b),
                neg(c),
                clean(d),
            );
        }
    }
    let _ = write!(out, "{}</text>", escape_xml(text));
    out
}

/// The points a single-line text's estimated box is taken through, in the
/// entity's coordinates: its anchor, and -- when it shows any character --
/// the four corners of a box [`CHAR_ADVANCE_EM`] per character wide and the
/// height of its capitals tall (descenders are not counted), hung from the
/// anchor the way it is drawn and taken through the text's own axes, so a
/// stretched, slanted or turned text is measured as it is drawn. Glyph
/// widths come from the font the rasterizer picks, not from here, so the
/// width is an estimate; the anchor is exact.
fn text_box_points(layout: &TextLayout, text: &str, cap_height: f64) -> Vec<Point2D> {
    let Anchor { at, anchor, drop } = layout.anchor;
    let mut points = vec![at];
    let chars = text.chars().count();
    if chars == 0 {
        return points;
    }
    let h = layout.height;
    let width = CHAR_ADVANCE_EM / cap_height * h * chars as f64;
    let (u0, u1) = match anchor {
        "middle" => (-width / 2.0, width / 2.0),
        "end" => (-width, 0.0),
        _ => (0.0, width),
    };
    let (v0, v1) = (-drop * h, -drop * h + h);
    let [a, b, c, d] = layout.axes;
    for (u, v) in [(u0, v0), (u1, v0), (u1, v1), (u0, v1)] {
        points.push(Point2D {
            x: at.x + a * u + c * v,
            y: at.y + b * u + d * v,
        });
    }
    points
}

/// Counts a single-line text's estimated box (see [`text_box_points`])
/// towards the extent, and returns it in world coordinates.
pub(super) fn consider_text_box(layout: &TextLayout, text: &str, ctx: &mut Ctx) -> Option<Box2D> {
    let points = text_box_points(layout, text, ctx.cap_height);
    ctx.consider_all(&points);
    ctx.world_box(&points)
}

/// A single-line text's estimated box (see [`text_box_points`]) in world
/// coordinates, without counting it towards the extent.
pub(super) fn estimate_text_box(layout: &TextLayout, text: &str, ctx: &Ctx) -> Option<Box2D> {
    ctx.world_box(&text_box_points(layout, text, ctx.cap_height))
}

/// How big an MTEXT's block is, for its box: as wide and as tall as the
/// application that wrote the file measured it (DXF 42/43), when the file
/// says; otherwise as wide as its reference rectangle (DXF 41), which the
/// text is wrapped to, when it has one, else [`CHAR_ADVANCE_EM`] per
/// character of its longest line; and as tall as the lines are drawn --
/// the first line's capitals down to the last baseline, `block` -- when no
/// height is stated.
pub(super) struct MTextBlock {
    pub(super) width: f64,
    pub(super) height: f64,
}

impl MTextBlock {
    pub(super) fn new(
        extents: (Option<f64>, Option<f64>),
        rect_width: f64,
        longest_line: usize,
        text_height: f64,
        block: f64,
        cap_height: f64,
    ) -> MTextBlock {
        let stated = |v: Option<f64>| v.filter(|v| v.is_finite() && *v > 0.0);
        let width = stated(extents.0)
            .or(stated(Some(rect_width)))
            .unwrap_or(CHAR_ADVANCE_EM / cap_height * text_height * longest_line as f64);
        MTextBlock {
            width,
            height: stated(extents.1).unwrap_or(block),
        }
    }
}

/// Counts an MTEXT's box towards the extent, with its insertion point, and
/// returns it in world coordinates: the block hung from the insertion point
/// the way the text is drawn from it -- the attachment's column says which
/// of its left edge, middle or right edge the point is on, its row which of
/// its top, middle or bottom, and a block with no stated attachment has its
/// first baseline on the point (`text_height` below the block's top) and
/// runs right -- turned by the text's rotation about the point.
pub(super) fn consider_mtext_box(
    at: Point2D,
    rotation: f64,
    attachment: Option<MTextAttachment>,
    block: &MTextBlock,
    text_height: f64,
    ctx: &mut Ctx,
) -> Option<Box2D> {
    use MTextAttachment as A;
    let mut points = vec![at];
    let (w, h) = (block.width, block.height);
    let (u0, u1) = match attachment {
        Some(A::TopCenter | A::MiddleCenter | A::BottomCenter) => (-w / 2.0, w / 2.0),
        Some(A::TopRight | A::MiddleRight | A::BottomRight) => (-w, 0.0),
        _ => (0.0, w),
    };
    let (v0, v1) = match attachment {
        None => (text_height - h, text_height),
        Some(A::TopLeft | A::TopCenter | A::TopRight) => (-h, 0.0),
        Some(A::MiddleLeft | A::MiddleCenter | A::MiddleRight) => (-h / 2.0, h / 2.0),
        Some(A::BottomLeft | A::BottomCenter | A::BottomRight) => (0.0, h),
    };
    let (sin, cos) = rotation.sin_cos();
    for (u, v) in [(u0, v0), (u1, v0), (u1, v1), (u0, v1)] {
        points.push(Point2D {
            x: at.x + cos * u - sin * v,
            y: at.y + sin * u + cos * v,
        });
    }
    ctx.consider_all(&points);
    ctx.world_box(&points)
}

#[cfg(test)]
mod tests {
    use super::*;
    use HorizontalJustification as H;
    use VerticalJustification as V;

    fn p(x: f64, y: f64) -> Point2D {
        Point2D { x, y }
    }

    #[test]
    fn a_left_baseline_text_hangs_from_its_start_point() {
        let a = anchor(p(1.0, 2.0), None, H::Left, V::Baseline, 0.3);
        assert_eq!(
            a,
            Anchor {
                at: p(1.0, 2.0),
                anchor: "start",
                drop: 0.0
            }
        );
    }

    #[test]
    fn each_justification_takes_its_anchor_and_baseline() {
        let (start, align) = (p(0.0, 0.0), Some(p(10.0, 4.0)));
        let at = |h, v| anchor(start, align, h, v, 0.3);
        assert_eq!(at(H::Center, V::Baseline).anchor, "middle");
        assert_eq!(at(H::Center, V::Baseline).at, p(10.0, 4.0));
        assert_eq!(at(H::Right, V::Top).anchor, "end");
        assert_eq!(at(H::Right, V::Top).drop, 1.0);
        assert_eq!(at(H::Left, V::Bottom).drop, -0.3);
        assert_eq!(at(H::Left, V::Middle).drop, 0.5);
        // MIDDLE is centred both ways, whatever the vertical says.
        assert_eq!(at(H::Middle, V::Baseline).drop, 0.5);
        // ALIGNED and FIT are centred between their two points.
        let aligned = at(H::Aligned, V::Baseline);
        assert_eq!((aligned.at, aligned.anchor), (p(5.0, 2.0), "middle"));
        assert_eq!(at(H::Fit, V::Baseline).at, p(5.0, 2.0));
    }

    #[test]
    fn a_justification_without_its_point_falls_back_to_the_start_point() {
        let a = anchor(p(1.0, 2.0), None, H::Right, V::Top, 0.3);
        assert_eq!((a.at, a.anchor, a.drop), (p(1.0, 2.0), "start", 0.0));
    }

    #[test]
    fn the_text_axes_stretch_along_the_baseline_and_slant_up_the_characters() {
        let close = |a: [f64; 4], b: [f64; 4]| {
            assert!(
                a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-12),
                "{a:?} vs {b:?}"
            )
        };
        close(text_axes(0.0, 0.0, 1.0), [1.0, 0.0, 0.0, 1.0]);
        close(text_axes(0.0, 0.0, 0.5), [0.5, 0.0, 0.0, 1.0]);
        // 45 degrees of slant: the top of a character one unit tall is one
        // unit further along the baseline.
        close(
            text_axes(0.0, std::f64::consts::FRAC_PI_4, 1.0),
            [1.0, 0.0, 1.0, 1.0],
        );
        // A quarter turn: the baseline runs up, the characters point left.
        close(
            text_axes(std::f64::consts::FRAC_PI_2, 0.0, 2.0),
            [0.0, 2.0, -1.0, 0.0],
        );
    }

    #[test]
    fn a_width_factor_of_zero_or_not_a_number_is_drawn_at_one() {
        assert_eq!(effective_width_factor(0.0), 1.0);
        assert_eq!(effective_width_factor(f64::NAN), 1.0);
        assert_eq!(effective_width_factor(0.8), 0.8);
        assert_eq!(effective_width_factor(-1.0), -1.0);
    }
}
