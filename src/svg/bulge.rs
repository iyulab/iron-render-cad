//! Polyline arc segments: a vertex's bulge turned into the circular arc it
//! describes.
//!
//! A bulge is the tangent of a quarter of the arc's included angle, positive
//! when the arc runs counter-clockwise from its start vertex to its end
//! vertex (so the arc lies to the right of the chord's direction). That is
//! all the format says, and it fixes the arc: this is arithmetic on the two
//! vertices and the bulge, with nothing to choose.

use std::f64::consts::FRAC_PI_2;
use uncad_model::model::{Point2D, PolylineVertex};

/// The arc a bulged segment describes, in the polyline's own coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct BulgeArc {
    pub center: Point2D,
    pub radius: f64,
    /// Radians: the direction from the center to the segment's start vertex.
    pub start_angle: f64,
    /// Radians, signed: positive is counter-clockwise. Its magnitude is below
    /// a full turn and above a half turn exactly when `|bulge| > 1`.
    pub sweep: f64,
}

impl BulgeArc {
    /// The point of the arc at `angle` (radians).
    pub fn at(&self, angle: f64) -> Point2D {
        Point2D {
            x: self.center.x + self.radius * angle.cos(),
            y: self.center.y + self.radius * angle.sin(),
        }
    }

    /// The arc's axis-aligned extreme points that lie within its sweep -- the
    /// points besides its two ends that reach furthest in x or y.
    pub fn extremes(&self) -> Vec<Point2D> {
        (0..4)
            .map(|k| f64::from(k) * FRAC_PI_2)
            .filter(|&a| self.contains_angle(a))
            .map(|a| self.at(a))
            .collect()
    }

    fn contains_angle(&self, angle: f64) -> bool {
        // How far along the sweep's direction `angle` is from the start, in [0, 2pi).
        let along = (angle - self.start_angle) * self.sweep.signum();
        along.rem_euclid(std::f64::consts::TAU) <= self.sweep.abs()
    }
}

/// The arc from `from` to `to` with the given `bulge`, or `None` when the
/// segment is straight: a zero or non-finite bulge, or two coincident
/// vertices (no chord, so no arc).
pub(super) fn bulge_arc(from: Point2D, to: Point2D, bulge: f64) -> Option<BulgeArc> {
    if bulge == 0.0 || !bulge.is_finite() {
        return None;
    }
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let chord = dx.hypot(dy);
    if chord == 0.0 || !chord.is_finite() {
        return None;
    }
    // With the sagitta s = bulge * chord / 2, the center sits on the chord's
    // perpendicular bisector at (chord / 4) * (1 / bulge - bulge) to the
    // left of the chord's direction; the radius is (chord / 4) * (1 / bulge +
    // bulge) in magnitude.
    let offset = chord / 4.0 * (1.0 / bulge - bulge);
    let (nx, ny) = (-dy / chord, dx / chord);
    let center = Point2D {
        x: (from.x + to.x) / 2.0 + nx * offset,
        y: (from.y + to.y) / 2.0 + ny * offset,
    };
    Some(BulgeArc {
        center,
        radius: (chord / 4.0 * (1.0 / bulge + bulge)).abs(),
        start_angle: (from.y - center.y).atan2(from.x - center.x),
        sweep: 4.0 * bulge.atan(),
    })
}

/// A polyline's segments in order, each as its two ends and its arc when it
/// has one: the segment leaving each vertex, and for a closed polyline also
/// the one from the last vertex back to the first.
pub(super) fn segments(
    vertices: &[PolylineVertex],
    closed: bool,
) -> impl Iterator<Item = (Point2D, Point2D, Option<BulgeArc>)> + '_ {
    let n = vertices.len();
    let count = match n {
        0 | 1 => 0,
        _ if closed => n,
        _ => n - 1,
    };
    (0..count).map(move |i| {
        let (from, to) = (vertices[i], vertices[(i + 1) % n]);
        (
            from.point,
            to.point,
            bulge_arc(from.point, to.point, from.bulge),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f64, y: f64) -> Point2D {
        Point2D { x, y }
    }

    fn close(a: Point2D, b: Point2D) -> bool {
        (a.x - b.x).abs() < 1e-12 && (a.y - b.y).abs() < 1e-12
    }

    #[test]
    fn a_bulge_of_one_is_a_half_circle_to_the_right_of_the_chord() {
        // Counter-clockwise from (0, 0) to (2, 0) around (1, 0) passes below.
        let arc = bulge_arc(p(0.0, 0.0), p(2.0, 0.0), 1.0).unwrap();
        assert!(close(arc.center, p(1.0, 0.0)), "{arc:?}");
        assert!((arc.radius - 1.0).abs() < 1e-12);
        assert!((arc.sweep - std::f64::consts::PI).abs() < 1e-12);
        assert!(close(arc.at(arc.start_angle), p(0.0, 0.0)));
        assert!(close(arc.at(arc.start_angle + arc.sweep), p(2.0, 0.0)));
        assert!(close(
            arc.at(arc.start_angle + arc.sweep / 2.0),
            p(1.0, -1.0)
        ));
    }

    #[test]
    fn a_negative_bulge_runs_clockwise_on_the_other_side() {
        let arc = bulge_arc(p(0.0, 0.0), p(2.0, 0.0), -1.0).unwrap();
        assert!(close(
            arc.at(arc.start_angle + arc.sweep / 2.0),
            p(1.0, 1.0)
        ));
        assert!(close(arc.at(arc.start_angle + arc.sweep), p(2.0, 0.0)));
    }

    #[test]
    fn a_smaller_bulge_is_a_shallower_arc_with_its_sagitta_as_the_format_defines() {
        // Sagitta = bulge * chord / 2 = 0.5, on the right of (0,0) -> (2,0).
        let arc = bulge_arc(p(0.0, 0.0), p(2.0, 0.0), 0.5).unwrap();
        assert!(close(arc.center, p(1.0, 0.75)), "{arc:?}");
        assert!((arc.radius - 1.25).abs() < 1e-12);
        assert!(close(
            arc.at(arc.start_angle + arc.sweep / 2.0),
            p(1.0, -0.5)
        ));
    }

    #[test]
    fn straight_and_degenerate_segments_have_no_arc() {
        assert!(bulge_arc(p(0.0, 0.0), p(2.0, 0.0), 0.0).is_none());
        assert!(bulge_arc(p(1.0, 1.0), p(1.0, 1.0), 0.5).is_none());
        assert!(bulge_arc(p(0.0, 0.0), p(2.0, 0.0), f64::NAN).is_none());
    }

    #[test]
    fn the_extremes_are_the_axis_points_within_the_sweep_only() {
        // The lower half circle reaches y = -1 at x = 1 and nothing else
        // beyond its ends.
        let arc = bulge_arc(p(0.0, 0.0), p(2.0, 0.0), 1.0).unwrap();
        let ex = arc.extremes();
        assert!(ex.iter().any(|e| close(*e, p(1.0, -1.0))), "{ex:?}");
        assert!(ex.iter().all(|e| e.y <= 1e-12), "{ex:?}");
    }

    #[test]
    fn a_closed_polyline_has_a_segment_back_to_its_first_vertex_with_the_last_bulge() {
        let v = [
            PolylineVertex::straight(p(0.0, 0.0)),
            PolylineVertex::straight(p(2.0, 0.0)),
            PolylineVertex {
                point: p(2.0, 2.0),
                bulge: 1.0,
            },
        ];
        let open: Vec<_> = segments(&v, false).collect();
        assert_eq!(open.len(), 2);
        let closed: Vec<_> = segments(&v, true).collect();
        assert_eq!(closed.len(), 3);
        let (from, to, arc) = closed[2];
        assert_eq!((from, to), (p(2.0, 2.0), p(0.0, 0.0)));
        assert!(arc.is_some());
    }
}
