//! Mesh-based hydrostatic geometry: tessellation of the body into a closed
//! triangle list, and clipping of that list against the water plane.
//!
//! These helpers are shape-agnostic past `frustum_triangles` — the buoyancy
//! methods consume only a `&[[Vec3; 3]]` closed mesh (outward winding), so an
//! arbitrary imported mesh can replace the frustum without touching them.
//!
//! Key hydrostatics facts the methods rely on (both exact, no waterline "cap"
//! polygon needed as long as the input mesh is closed):
//! - Integrating pressure `p = ρ g (level − y)` over the *clipped* (submerged)
//!   body surface gives exactly the buoyant force ρ g V_sub and the correct
//!   torque, because the missing cap lies at the surface where p = 0.
//! - By the divergence theorem with F = (0, y − level, 0), the submerged volume
//!   is `V = Σ (ȳ − level) · n_y · A` over the clipped triangles, and the cap
//!   again contributes zero (y = level there). The same trick yields the centre
//!   of buoyancy from second moments, cap-free.

use bevy::prelude::*;

/// Point on a horizontal ring of radius `r` at height `y`, at angle `i/n` of a
/// full turn. The one place the crate's ring convention (angle direction /
/// axis) is defined — the render mesh, the collider hull, and this module's
/// tessellation all sample rims through it, so they can never drift apart.
pub fn ring_point(i: usize, n: usize, y: f32, r: f32) -> Vec3 {
    let a = i as f32 / n as f32 * std::f32::consts::TAU;
    let (s, c) = a.sin_cos();
    Vec3::new(c * r, y, s * r)
}

/// Transform a local-space triangle by a body pose (rotation pre-expanded to a
/// matrix — cheaper than `Quat * Vec3` when transforming many points).
pub fn tri_to_world(tri: &[Vec3; 3], pos: Vec3, rot: &Mat3) -> [Vec3; 3] {
    [pos + *rot * tri[0], pos + *rot * tri[1], pos + *rot * tri[2]]
}

/// Closed triangle mesh of a conical frustum in local space (axis = +Y,
/// `radius_top` at +length/2). `radial` segments around, `rings` bands along the
/// wall (more bands = finer waterline clipping on a tilted body). Winding is
/// counter-clockwise seen from outside (outward normals).
pub fn frustum_triangles(
    r_top: f32,
    r_bottom: f32,
    length: f32,
    radial: usize,
    rings: usize,
) -> Vec<[Vec3; 3]> {
    let radial = radial.max(3);
    let rings = rings.max(1);
    let mut tris = Vec::with_capacity(radial * rings * 2 + radial * 2);

    let point = |i: usize, y: f32, r: f32| ring_point(i, radial, y, r);

    // Side wall: quads between consecutive rings, split into two triangles.
    for j in 0..rings {
        let (t0, t1) = (j as f32 / rings as f32, (j + 1) as f32 / rings as f32);
        let (y0, y1) = (length * (t0 - 0.5), length * (t1 - 0.5));
        let (r0, r1) = (
            r_bottom + (r_top - r_bottom) * t0,
            r_bottom + (r_top - r_bottom) * t1,
        );
        for i in 0..radial {
            let p00 = point(i, y0, r0);
            let p01 = point(i, y1, r1);
            let p10 = point(i + 1, y0, r0);
            let p11 = point(i + 1, y1, r1);
            tris.push([p00, p01, p10]);
            tris.push([p10, p01, p11]);
        }
    }

    // End caps (skipped if the rim is degenerate).
    let top_c = Vec3::new(0.0, length * 0.5, 0.0);
    let bot_c = Vec3::new(0.0, -length * 0.5, 0.0);
    for i in 0..radial {
        if r_top > 1e-4 {
            tris.push([
                top_c,
                point(i + 1, length * 0.5, r_top),
                point(i, length * 0.5, r_top),
            ]);
        }
        if r_bottom > 1e-4 {
            tris.push([
                bot_c,
                point(i, -length * 0.5, r_bottom),
                point(i + 1, -length * 0.5, r_bottom),
            ]);
        }
    }
    tris
}

/// Clip a (world-space) triangle against the water plane, keeping the part with
/// `y <= level`. Appends 0, 1, or 2 triangles to `out`, preserving winding.
pub fn clip_triangle_below(tri: [Vec3; 3], level: f32, out: &mut Vec<[Vec3; 3]>) {
    // Sutherland–Hodgman against a single plane: walk the edges, keep submerged
    // vertices and edge/plane intersections. Yields a 0/3/4-gon.
    let mut poly = [Vec3::ZERO; 4];
    let mut n = 0;
    for i in 0..3 {
        let (a, b) = (tri[i], tri[(i + 1) % 3]);
        let (da, db) = (a.y - level, b.y - level);
        if da <= 0.0 {
            poly[n] = a;
            n += 1;
        }
        if (da <= 0.0) != (db <= 0.0) {
            let t = da / (da - db); // da != db when signs differ
            poly[n] = a.lerp(b, t);
            n += 1;
        }
    }
    if n >= 3 {
        out.push([poly[0], poly[1], poly[2]]);
    }
    if n == 4 {
        out.push([poly[0], poly[2], poly[3]]);
    }
}

/// Area-weighted quantities of one planar triangle.
pub struct TriGeom {
    /// Outward unit normal (from the mesh winding).
    pub normal: Vec3,
    pub area: f32,
    pub centroid: Vec3,
}

pub fn tri_geom(tri: &[Vec3; 3]) -> Option<TriGeom> {
    let cross = (tri[1] - tri[0]).cross(tri[2] - tri[0]);
    let double_area = cross.length();
    if double_area < 1e-9 {
        return None;
    }
    Some(TriGeom {
        normal: cross / double_area,
        area: double_area * 0.5,
        centroid: (tri[0] + tri[1] + tri[2]) / 3.0,
    })
}

/// Volume-only variant of [`submerged_volume_centroid`] — the same volume
/// term, without the second-moment accumulation (which costs ~3× the flops)
/// for callers that never read the centroid, like a bisection loop.
pub fn submerged_volume(clipped: &[[Vec3; 3]], level: f32) -> f32 {
    let mut volume = 0.0;
    for tri in clipped {
        let cross = (tri[1] - tri[0]).cross(tri[2] - tri[0]);
        if cross.length_squared() < 1e-18 {
            continue;
        }
        volume += ((tri[0].y + tri[1].y + tri[2].y) / 3.0 - level) * (cross * 0.5).y;
    }
    volume
}

/// Exact submerged volume + centre of buoyancy of a clipped triangle list
/// (divergence theorem; see the module docs for why no cap polygon is needed).
/// Returns `None` when effectively nothing is submerged.
pub fn submerged_volume_centroid(clipped: &[[Vec3; 3]], level: f32) -> Option<(f32, Vec3)> {
    let mut volume = 0.0;
    let mut moment = Vec3::ZERO;
    for tri in clipped {
        // Every term below multiplies the unit normal by the area, so use the
        // raw area-weighted normal (cross/2) directly — no sqrt/normalize.
        let cross = (tri[1] - tri[0]).cross(tri[2] - tri[0]);
        if cross.length_squared() < 1e-18 {
            continue;
        }
        let area_normal = cross * 0.5;
        let (p0, p1, p2) = (tri[0], tri[1], tri[2]);
        volume += ((p0.y + p1.y + p2.y) / 3.0 - level) * area_normal.y;
        // ∫ q²/2 · n_q dA per axis, with mean(q²) over a triangle =
        // (q0²+q1²+q2²+q0q1+q0q2+q1q2)/6; y is taken relative to the surface.
        let sq = |q0: f32, q1: f32, q2: f32| {
            (q0 * q0 + q1 * q1 + q2 * q2 + q0 * q1 + q0 * q2 + q1 * q2) / 6.0
        };
        moment.x += 0.5 * sq(p0.x, p1.x, p2.x) * area_normal.x;
        moment.y += 0.5 * sq(p0.y - level, p1.y - level, p2.y - level) * area_normal.y;
        moment.z += 0.5 * sq(p0.z, p1.z, p2.z) * area_normal.z;
    }
    if volume < 1e-9 {
        return None;
    }
    let mut c = moment / volume;
    c.y += level; // y-moment was computed surface-relative
    Some((volume, c))
}
