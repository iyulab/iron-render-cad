//! Object coordinate systems: taking a planar entity's coordinates from the
//! plane its file states them in to the world, seen from above.
//!
//! CIRCLE, ARC, LWPOLYLINE and POLYLINE_2D, TEXT, ATTRIB, SOLID and TRACE
//! state their points in an *object coordinate system* (OCS) -- the plane
//! whose normal is the entity's `extrusion`, its axes chosen by the DXF
//! reference's arbitrary axis algorithm -- and the model carries them as
//! stated. The renderer draws in plan, so it takes them to the world's
//! (x, y) itself. An INSERT's placement is the model's own arithmetic
//! ([`Affine2::from_insert`]), which already does the same for the block.
//!
//! Three cases, because a curve drawn exactly in one is not the same kind
//! of curve in another:
//! - the plane is the world's (normal (0, 0, 1)): nothing to do, and the
//!   entity is drawn exactly as it always was;
//! - the plane is the world's seen from behind (normal (0, 0, -1), a
//!   mirrored entity): x changes sign, so a counter-clockwise arc runs
//!   clockwise and a text reads backwards, and every curve stays the curve
//!   it was;
//! - the plane is tilted: seen from above, a circle is an ellipse, so a
//!   curve is drawn through points of its outline.
//!
//! A normal within 1e-9 (relatively) of the z axis is taken as the z axis:
//! files write it with rounding noise in the other two components.

use uncad_model::{Affine2, Point2D, Point3D};

/// The DXF reference's threshold in its arbitrary axis algorithm: a normal
/// this close to the world z axis takes its OCS x axis from the world y
/// axis, any other from the world z axis.
const ARBITRARY_AXIS_THRESHOLD: f64 = 1.0 / 64.0;

/// Which plane an entity's coordinates are stated in, as far as drawing it
/// in plan is concerned.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Ocs {
    /// The world's own plane: the coordinates are world coordinates.
    World,
    /// The world's plane seen from behind: `(x, y) -> (-x, y)`.
    Mirrored,
    /// A tilted plane: the map from its `(x, y)` to the world's, at the
    /// entity's height in it.
    Tilted(Affine2),
}

impl Ocs {
    /// The plane whose normal is `extrusion`, at height `z` in it (a
    /// circle's centre's z, a polyline's or a text's elevation). A normal
    /// of zero length, or one that is not finite, is not a direction: it is
    /// drawn as the z axis would be, the way the model places a block
    /// reference.
    pub(super) fn new(extrusion: Point3D, z: f64) -> Ocs {
        let Some(n) = normalized(extrusion) else {
            return Ocs::World;
        };
        if n.x.abs().max(n.y.abs()) <= 1e-9 * n.z.abs() {
            return if n.z > 0.0 { Ocs::World } else { Ocs::Mirrored };
        }
        let seed = if n.x.abs() < ARBITRARY_AXIS_THRESHOLD && n.y.abs() < ARBITRARY_AXIS_THRESHOLD {
            Point3D {
                x: 0.0,
                y: 1.0,
                z: 0.0,
            }
        } else {
            Point3D {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            }
        };
        let Some(x_axis) = normalized(cross(seed, n)) else {
            return Ocs::World;
        };
        let Some(y_axis) = normalized(cross(n, x_axis)) else {
            return Ocs::World;
        };
        Ocs::Tilted(Affine2 {
            a: x_axis.x,
            b: x_axis.y,
            c: y_axis.x,
            d: y_axis.y,
            e: n.x * z,
            f: n.y * z,
        })
    }

    /// The map from the plane's `(x, y)` to the world's.
    pub(super) fn map(&self) -> Affine2 {
        match self {
            Ocs::World => Affine2::IDENTITY,
            Ocs::Mirrored => Affine2 {
                a: -1.0,
                ..Affine2::IDENTITY
            },
            Ocs::Tilted(map) => *map,
        }
    }

    /// Where the plane's point `p` is in the world, seen from above.
    pub(super) fn apply(&self, p: Point2D) -> Point2D {
        match self {
            Ocs::World => p,
            Ocs::Mirrored => Point2D { x: -p.x, y: p.y },
            Ocs::Tilted(map) => map.apply(p),
        }
    }
}

/// Whether an entity stated in the plane of `extrusion` at height `z` has
/// real numbers for everything its plane is drawn from: the normal always,
/// and the height where the plane is tilted -- in the world's own plane,
/// or mirrored, the height moves nothing in plan.
pub(super) fn numbers_are_real(extrusion: &Point3D, z: f64) -> bool {
    [extrusion.x, extrusion.y, extrusion.z]
        .iter()
        .all(|v| v.is_finite())
        && (z.is_finite() || !matches!(Ocs::new(*extrusion, 0.0), Ocs::Tilted(_)))
}

fn normalized(v: Point3D) -> Option<Point3D> {
    let len = (v.x * v.x + v.y * v.y + v.z * v.z).sqrt();
    (len.is_finite() && len > 0.0).then(|| Point3D {
        x: v.x / len,
        y: v.y / len,
        z: v.z / len,
    })
}

fn cross(u: Point3D, v: Point3D) -> Point3D {
    Point3D {
        x: u.y * v.z - u.z * v.y,
        y: u.z * v.x - u.x * v.z,
        z: u.x * v.y - u.y * v.x,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uncad_model::model::{Confidence, EntityCommon, EntityId, InsertEntity, Origin, Ref};

    fn xyz(x: f64, y: f64, z: f64) -> Point3D {
        Point3D { x, y, z }
    }

    #[test]
    fn the_z_axis_is_the_world_and_its_opposite_a_mirror() {
        assert_eq!(Ocs::new(xyz(0.0, 0.0, 1.0), 5.0), Ocs::World);
        assert_eq!(Ocs::new(xyz(0.0, 0.0, 3.0), 5.0), Ocs::World);
        assert_eq!(Ocs::new(xyz(1e-17, -2e-17, 1.0), 5.0), Ocs::World);
        assert_eq!(Ocs::new(xyz(0.0, 0.0, -1.0), 5.0), Ocs::Mirrored);
        assert_eq!(Ocs::new(xyz(1e-17, 0.0, -1.0), 5.0), Ocs::Mirrored);
        let p = Point2D { x: 3.0, y: 4.0 };
        assert_eq!(Ocs::Mirrored.apply(p), Point2D { x: -3.0, y: 4.0 });
        // Not a direction at all: drawn as the z axis.
        assert_eq!(Ocs::new(xyz(0.0, 0.0, 0.0), 5.0), Ocs::World);
        assert_eq!(Ocs::new(xyz(f64::NAN, 0.0, 1.0), 5.0), Ocs::World);
    }

    #[test]
    fn a_tilted_plane_follows_the_arbitrary_axis_algorithm() {
        // Normal (1, 0, 0): its x axis is z x N = (0, 1, 0) and its y axis
        // N x (0, 1, 0) = (0, 0, 1), so (x, y) at height z is the world
        // point (z, x, y) -- seen from above, (z, x).
        let ocs = Ocs::new(xyz(1.0, 0.0, 0.0), 7.0);
        let p = ocs.apply(Point2D { x: 2.0, y: 3.0 });
        assert!(
            (p.x - 7.0).abs() < 1e-12 && (p.y - 2.0).abs() < 1e-12,
            "{p:?}"
        );
    }

    #[test]
    fn the_map_is_the_one_the_model_places_a_block_reference_through() {
        // An INSERT at the plane's origin, unscaled and unturned, is placed
        // by exactly the plane's map: the two cannot disagree.
        for normal in [
            xyz(0.0, 0.0, -1.0),
            xyz(1.0, 0.0, 1.0),
            xyz(0.3, -0.2, 0.9),
            xyz(0.01, 0.005, 1.0),
            xyz(0.0, 1.0, 0.0),
        ] {
            let z = 2.5;
            let insert = InsertEntity {
                common: EntityCommon {
                    id: EntityId::new(1),
                    origin: Origin::Vector,
                    confidence: Confidence::High,
                    source_handle: Ref::Absent,
                    layer: Ref::Absent,
                    color_index: 7,
                    true_color: None,
                    invisible: false,
                },
                block_name: Ref::Absent,
                insertion_point: xyz(0.0, 0.0, z),
                scale: xyz(1.0, 1.0, 1.0),
                rotation: 0.0,
                attribs: Vec::new(),
                extrusion: normal,
            };
            let model = Affine2::from_insert(&insert);
            let ours = Ocs::new(normal, z).map();
            for (a, b) in [
                (model.a, ours.a),
                (model.b, ours.b),
                (model.c, ours.c),
                (model.d, ours.d),
                (model.e, ours.e),
                (model.f, ours.f),
            ] {
                assert!((a - b).abs() < 1e-12, "{normal:?}: {model:?} vs {ours:?}");
            }
        }
    }

    #[test]
    fn a_tilted_planes_height_has_to_be_a_number_and_a_flat_ones_does_not() {
        assert!(numbers_are_real(&xyz(0.0, 0.0, 1.0), f64::NAN));
        assert!(numbers_are_real(&xyz(0.0, 0.0, -1.0), f64::NAN));
        assert!(!numbers_are_real(&xyz(1.0, 0.0, 1.0), f64::NAN));
        assert!(!numbers_are_real(&xyz(0.0, f64::INFINITY, 1.0), 0.0));
    }
}
