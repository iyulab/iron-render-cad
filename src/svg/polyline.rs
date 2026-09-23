//! LWPOLYLINE and POLYLINE_2D segments that are arcs: what a vertex's bulge
//! describes, drawn as an exact SVG arc and measured by the arc's own box.
//!
//! The bulge on vertex `i` belongs to the segment from vertex `i` to vertex
//! `i + 1` (the closing segment's is on the last vertex of a closed
//! polyline) and is `tan(theta / 4)` for the arc's included angle `theta`:
//! positive is counter-clockwise from the first vertex to the second, 0 a
//! straight segment, 1 a half circle.

use super::format::clean;
use super::{arc_extent, polyline_element, Ctx};
use crate::limits::MAX_WORLD_COORDINATE;
use std::fmt::Write as _;
use uncad_model::model::Point2D;

/// The circular arc a bulge describes between two vertices.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct BulgeArc {
    pub(super) center: Point2D,
    pub(super) radius: f64,
    /// The angle of the arc's first point about the centre, radians.
    pub(super) start_angle: f64,
    /// The included angle, radians: positive counter-clockwise.
    pub(super) sweep: f64,
}

/// The arc from `from` to `to` with this bulge, or `None` for a straight
/// segment: a bulge of 0, one that is not a number, or two coincident
/// points (which no arc joins).
pub(super) fn bulge_arc(from: Point2D, to: Point2D, bulge: f64) -> Option<BulgeArc> {
    if bulge == 0.0 || !bulge.is_finite() {
        return None;
    }
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let chord = dx.hypot(dy);
    if chord.is_nan() || chord <= 1e-12 {
        return None;
    }
    let sweep = 4.0 * bulge.atan();
    let radius = chord * (1.0 + bulge * bulge) / (4.0 * bulge.abs());
    // The centre is on the chord's perpendicular bisector, to the left of
    // travel for a counter-clockwise arc, `radius - sagitta` from the
    // chord (a negative distance past a half circle: the bulge's own side).
    let sagitta = chord * bulge.abs() / 2.0;
    let offset = (radius - sagitta) * bulge.signum();
    let center = Point2D {
        x: (from.x + to.x) / 2.0 - dy / chord * offset,
        y: (from.y + to.y) / 2.0 + dx / chord * offset,
    };
    Some(BulgeArc {
        center,
        radius,
        start_angle: (from.y - center.y).atan2(from.x - center.x),
        sweep,
    })
}

/// One segment of a polyline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Segment {
    Line { to: Point2D },
    Arc { to: Point2D, arc: BulgeArc },
}

/// A closed polyline whose file repeats its first vertex at the end: the
/// repeat is dropped, so it does not make a segment of no length (and eat
/// the closing segment's bulge).
fn without_closing_repeat(vertices: &[Point2D], closed: bool) -> &[Point2D] {
    match vertices {
        [first, .., last] if closed && vertices.len() > 2 && first == last => {
            &vertices[..vertices.len() - 1]
        }
        _ => vertices,
    }
}

/// The segments of a polyline, in order: `bulges[i]` (0 where the list is
/// shorter) shapes the segment leaving vertex `i`, and a closed polyline
/// gets the segment from its last vertex back to its first.
pub(super) fn segments(vertices: &[Point2D], bulges: &[f64], closed: bool) -> Vec<Segment> {
    let vertices = without_closing_repeat(vertices, closed);
    let n = vertices.len();
    if n < 2 {
        return Vec::new();
    }
    let count = if closed { n } else { n - 1 };
    (0..count)
        .map(|i| {
            let (from, to) = (vertices[i], vertices[(i + 1) % n]);
            match bulge_arc(from, to, bulges.get(i).copied().unwrap_or(0.0)) {
                Some(arc) => Segment::Arc { to, arc },
                None => Segment::Line { to },
            }
        })
        .collect()
}

/// Whether an arc of this radius can be handed to the rasterizer as an arc.
/// A bulge of 1e-160 over a hundred-unit segment is an arc of radius 1e238:
/// the arc-to-curve conversion then works at a scale that has nothing to do
/// with the segment's, and a fuzzed drawing with one such vertex had not
/// finished rasterizing after five minutes. An arc that flat is its chord.
fn drawable_radius(radius: f64) -> bool {
    radius.is_finite() && radius < MAX_WORLD_COORDINATE
}

/// A polyline whose bulges are not all zero: a `<path>` of `L` commands for
/// its straight segments and `A` commands for its arcs, measured by its
/// vertices and each arc's own box.
///
/// The canvas is the drawing with y negated, so a counter-clockwise arc --
/// a positive bulge -- still turns counter-clockwise on screen, which in
/// SVG's own y-down frame is sweep flag 0 (the flag an ARC uses); a
/// negative bulge turns clockwise, sweep flag 1. Past a half circle
/// (`|bulge| > 1`) the arc is the large one.
pub(super) fn bulged_element(
    vertices: &[Point2D],
    bulges: &[f64],
    closed: bool,
    color: &str,
    ctx: &mut Ctx,
) -> String {
    let frame = ctx.frame;
    let segments = segments(vertices, bulges, closed);
    let Some(start) = without_closing_repeat(vertices, closed).first() else {
        return String::new();
    };
    ctx.consider_all(vertices);
    if segments.is_empty() {
        return polyline_element(vertices, closed, color, frame);
    }
    let mut d = format!("M {} {}", frame.x(start.x), frame.y(start.y));
    for segment in &segments {
        match *segment {
            Segment::Line { to } => {
                let _ = write!(d, " L {} {}", frame.x(to.x), frame.y(to.y));
            }
            Segment::Arc { to, arc } if drawable_radius(arc.radius) => {
                // The arc's own extent: counter-clockwise from its first
                // point, or -- for a clockwise one -- from its second.
                let from_angle = if arc.sweep >= 0.0 {
                    arc.start_angle
                } else {
                    arc.start_angle + arc.sweep
                };
                ctx.consider_box(&arc_extent(
                    arc.center,
                    arc.radius,
                    from_angle,
                    arc.sweep.abs(),
                ));
                let large = u8::from(arc.sweep.abs() > std::f64::consts::PI);
                let sweep = u8::from(arc.sweep < 0.0);
                let r = clean(arc.radius);
                let _ = write!(
                    d,
                    " A {r} {r} 0 {large} {sweep} {} {}",
                    frame.x(to.x),
                    frame.y(to.y)
                );
            }
            Segment::Arc { to, .. } => {
                let _ = write!(d, " L {} {}", frame.x(to.x), frame.y(to.y));
            }
        }
    }
    if closed {
        d.push_str(" Z");
    }
    format!("<path d=\"{d}\" fill=\"none\" stroke=\"{color}\"/>")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f64, y: f64) -> Point2D {
        Point2D { x, y }
    }

    /// The point `t` of the way along the arc.
    fn at(arc: &BulgeArc, t: f64) -> Point2D {
        let angle = arc.start_angle + arc.sweep * t;
        p(
            arc.center.x + arc.radius * angle.cos(),
            arc.center.y + arc.radius * angle.sin(),
        )
    }

    fn near(a: Point2D, b: Point2D) -> bool {
        (a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9
    }

    #[test]
    fn a_bulge_of_one_is_a_half_circle_on_the_side_its_sign_says() {
        // (0,0) -> (10,0), counter-clockwise: centre (5,0), through (5,-5).
        let ccw = bulge_arc(p(0.0, 0.0), p(10.0, 0.0), 1.0).expect("an arc");
        assert!(near(ccw.center, p(5.0, 0.0)), "{ccw:?}");
        assert!((ccw.radius - 5.0).abs() < 1e-12);
        assert!(near(at(&ccw, 0.5), p(5.0, -5.0)), "{ccw:?}");
        assert!(near(at(&ccw, 1.0), p(10.0, 0.0)));
        // Clockwise: the mirror image, through (5,5).
        let cw = bulge_arc(p(0.0, 0.0), p(10.0, 0.0), -1.0).expect("an arc");
        assert!(near(at(&cw, 0.5), p(5.0, 5.0)), "{cw:?}");
    }

    #[test]
    fn a_quarter_bulge_turns_a_quarter_circle() {
        // tan(22.5 degrees), clockwise: (40,50) -> (50,60) about (50,50).
        let arc = bulge_arc(p(40.0, 50.0), p(50.0, 60.0), -0.41421356237309503).unwrap();
        assert!(near(arc.center, p(50.0, 50.0)), "{arc:?}");
        assert!((arc.radius - 10.0).abs() < 1e-9);
        assert!((arc.sweep + std::f64::consts::FRAC_PI_2).abs() < 1e-12);
    }

    #[test]
    fn a_zero_a_non_number_or_a_point_is_a_straight_segment() {
        assert_eq!(bulge_arc(p(0.0, 0.0), p(1.0, 0.0), 0.0), None);
        assert_eq!(bulge_arc(p(0.0, 0.0), p(1.0, 0.0), f64::NAN), None);
        assert_eq!(bulge_arc(p(1.0, 1.0), p(1.0, 1.0), 1.0), None);
    }

    #[test]
    fn a_closed_polyline_that_repeats_its_first_vertex_keeps_its_closing_bulge() {
        // A DONUT written with its first vertex repeated at the end: two
        // half circles, not a half circle and a segment of no length.
        let segs = segments(
            &[p(0.0, 0.0), p(10.0, 0.0), p(0.0, 0.0)],
            &[1.0, 1.0, 0.0],
            true,
        );
        assert_eq!(segs.len(), 2);
        assert!(segs.iter().all(|s| matches!(s, Segment::Arc { .. })));
    }
}
