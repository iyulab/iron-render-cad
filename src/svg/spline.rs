//! SPLINE curves and HATCH spline edges as chords through points evaluated on
//! the curve.
//!
//! A spline stored by control points is a NURBS curve: its degree, knot
//! vector and weights define it, and the control points do not lie on it.
//! The model evaluates it ([`Nurbs`]); what is chosen here is how densely --
//! a fixed number of points per non-empty knot span. A spline stored by fit
//! points only has no knots to evaluate; it is drawn through the fit points,
//! which lie on the curve.

use uncad_model::model::{Point2D, Point3D, SplineEntity};
use uncad_model::Nurbs;

/// Points sampled per non-empty knot span. Fixed, so the same spline always
/// yields the same points.
const SAMPLES_PER_SPAN: usize = 16;

/// The points a SPLINE is drawn through, in order.
///
/// Control points whose knots and weights do not form a valid definition
/// (the knot count must be control points + degree + 1, and weights, when
/// present, one per control point) fall back to the control polygon -- the
/// only thing such data still says.
pub(super) fn spline_points(s: &SplineEntity) -> Vec<Point2D> {
    if let Some(points) = evaluate(s) {
        return points;
    }
    let source = if s.fit_points.is_empty() {
        &s.control_points
    } else {
        &s.fit_points
    };
    source.iter().map(|p| Point2D { x: p.x, y: p.y }).collect()
}

/// The curve sampled, or `None` when the spline does not carry a complete
/// NURBS definition.
fn evaluate(s: &SplineEntity) -> Option<Vec<Point2D>> {
    let control: Vec<Point2D> = s
        .control_points
        .iter()
        .map(|c| Point2D { x: c.x, y: c.y })
        .collect();
    nurbs_points(s.degree, &s.knots, &control, &s.weights)
}

/// A NURBS curve in the plane -- degree, knots, control points and weights
/// (empty for a non-rational one, where every weight is 1) -- sampled at
/// [`SAMPLES_PER_SPAN`] points per non-empty knot span, or `None` when those
/// do not define a curve (see [`Nurbs::new`]) or a sample falls at infinity.
/// Shared by SPLINE entities and HATCH spline edges.
pub(super) fn nurbs_points(
    degree: u32,
    knots: &[f64],
    control: &[Point2D],
    weights: &[f64],
) -> Option<Vec<Point2D>> {
    let curve = Nurbs::new(
        degree,
        knots,
        control.iter().map(|c| Point3D {
            x: c.x,
            y: c.y,
            z: 0.0,
        }),
        weights,
    )?;
    let mut points = Vec::new();
    let mut spans = curve.spans().peekable();
    while let Some((a, b)) = spans.next() {
        let count = if spans.peek().is_none() {
            SAMPLES_PER_SPAN + 1
        } else {
            SAMPLES_PER_SPAN
        };
        for i in 0..count {
            let u = a + (b - a) * (i as f64 / SAMPLES_PER_SPAN as f64);
            let q = curve.point_at(u)?;
            points.push(Point2D { x: q.x, y: q.y });
        }
    }
    (points.len() >= 2).then_some(points)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uncad_model::model::{Confidence, EntityCommon, EntityId, Origin, Point3D, Ref};

    fn spline(degree: u32, control: &[(f64, f64)], knots: &[f64], weights: &[f64]) -> SplineEntity {
        SplineEntity {
            start_tangent: None,
            end_tangent: None,
            common: EntityCommon {
                id: EntityId::new(1),
                origin: Origin::Vector,
                confidence: Confidence::High,
                source_handle: Ref::Absent,
                layer: Ref::Absent,
                color_index: 7,
                true_color: None,
                invisible: false,
                linetype: uncad_model::model::EntityLinetype::ByLayer,
                linetype_scale: 1.0,
                lineweight: Some(-1),
                transparency: Some(0),
            },
            degree,
            closed: Some(false),
            periodic: Some(false),
            knots: knots.to_vec(),
            weights: weights.to_vec(),
            fit_points: Vec::new(),
            control_points: control
                .iter()
                .map(|&(x, y)| Point3D { x, y, z: 0.0 })
                .collect(),
        }
    }

    #[test]
    fn a_rational_quadratic_with_the_standard_weights_is_an_exact_quarter_circle() {
        let h = std::f64::consts::FRAC_1_SQRT_2;
        let s = spline(
            2,
            &[(1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
            &[0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            &[1.0, h, 1.0],
        );
        let pts = spline_points(&s);
        assert_eq!(pts.len(), SAMPLES_PER_SPAN + 1);
        for p in &pts {
            assert!(
                (p.x.hypot(p.y) - 1.0).abs() < 1e-12,
                "{p:?} is off the unit circle"
            );
        }
        assert_eq!((pts[0].x, pts[0].y), (1.0, 0.0));
        let end = pts.last().unwrap();
        assert!((end.x - 0.0).abs() < 1e-15 && (end.y - 1.0).abs() < 1e-15);
    }

    #[test]
    fn a_clamped_cubic_passes_the_bezier_midpoint() {
        let c = [(0.0, 0.0), (1.0, 2.0), (3.0, 2.0), (4.0, 0.0)];
        let s = spline(3, &c, &[0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0], &[]);
        let pts = spline_points(&s);
        // u = 0.5 is sample 8 of 16; the Bezier midpoint is (P0 + 3P1 + 3P2 + P3) / 8.
        let mid = pts[SAMPLES_PER_SPAN / 2];
        assert!(
            (mid.x - 2.0).abs() < 1e-12 && (mid.y - 1.5).abs() < 1e-12,
            "{mid:?}"
        );
        assert_eq!((pts[0].x, pts[0].y), (0.0, 0.0));
        assert_eq!((pts.last().unwrap().x, pts.last().unwrap().y), (4.0, 0.0));
    }

    #[test]
    fn a_degree_one_spline_is_its_control_polygon() {
        let c = [(0.0, 0.0), (2.0, 0.0), (2.0, 2.0)];
        let s = spline(1, &c, &[0.0, 0.0, 1.0, 2.0, 2.0], &[]);
        let pts = spline_points(&s);
        // Every point lies on one of the two legs; both corners are hit exactly.
        for p in &pts {
            let on_first = p.y.abs() < 1e-12 && (0.0..=2.0).contains(&p.x);
            let on_second = (p.x - 2.0).abs() < 1e-12 && (0.0..=2.0).contains(&p.y);
            assert!(on_first || on_second, "{p:?} is off the polygon");
        }
        assert!(pts.iter().any(|p| (p.x, p.y) == (2.0, 0.0)));
        assert_eq!((pts.last().unwrap().x, pts.last().unwrap().y), (2.0, 2.0));
    }

    #[test]
    fn a_definition_that_does_not_add_up_falls_back_to_the_control_polygon() {
        let c = [(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)];
        // Three control points and degree 2 need six knots, not four.
        let s = spline(2, &c, &[0.0, 0.0, 1.0, 1.0], &[]);
        let pts = spline_points(&s);
        let expected: Vec<(f64, f64)> = c.to_vec();
        assert_eq!(pts.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>(), expected);
    }

    #[test]
    fn a_fit_point_spline_is_drawn_through_its_fit_points() {
        let mut s = spline(3, &[], &[], &[]);
        s.fit_points = vec![
            Point3D {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            Point3D {
                x: 1.0,
                y: 1.0,
                z: 0.0,
            },
        ];
        let pts = spline_points(&s);
        assert_eq!(pts.len(), 2);
        assert_eq!((pts[1].x, pts[1].y), (1.0, 1.0));
    }
}
