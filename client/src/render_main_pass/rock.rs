//! The asteroids' look: a procedurally carved boulder, built from the rock's replicated
//! seed so every client in the room sees the same stone.
//!
//! No shader. The other procedural surfaces here (metal, grass, magma) perturb a smooth
//! mesh's *colour*, because a machined part really is smooth and the interest is in its
//! finish. A rock is the opposite: its silhouette is the thing you read, and a fragment
//! shader cannot change a silhouette. So the noise goes into the **geometry** — an
//! icosphere displaced along its own normals — and the material stays a plain dull
//! `StandardMaterial`. That also means a rock costs no pipeline, no bind group, and no
//! WGSL to keep in sync with its Rust mirror; it is a mesh and nothing else.

use bad_spaceship_shared::net::splitmix64;
use bevy::asset::RenderAssetUsages;
use bevy::mesh::VertexAttributeValues;
use bevy::prelude::*;

/// Icosphere subdivisions. Three levels (1280 triangles) is enough that the displacement
/// reads as facets and crags rather than as a dented ball, and cheap enough that a room
/// running a full field of them is still drawing less than one monster.
const ROCK_SUBDIVISIONS: u32 = 3;

/// Displacement as a fraction of the rock's radius, per noise octave. The first octave is
/// the boulder's overall lumpy shape; the later ones chip it. They sum to well under 1, so
/// no vertex can ever be pushed through the centre and invert the surface.
const OCTAVES: [(f32, f32); 3] = [
    // (spatial frequency over the unit sphere, amplitude as a fraction of radius)
    (1.7, 0.26),
    (4.3, 0.11),
    (9.1, 0.05),
];

/// The render components for one asteroid, from its radius and replicated seed. The single
/// constructor, so a rock cannot look different on two clients watching the same flight.
pub fn asteroid_visual(
    radius: f32,
    seed: u32,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) -> (Mesh3d, MeshMaterial3d<StandardMaterial>) {
    (
        Mesh3d(meshes.add(rock_mesh(radius, seed))),
        MeshMaterial3d(materials.add(rock_material(seed))),
    )
}

/// Carve one rock: an icosphere whose every vertex is pushed in or out along its own
/// radius by layered value noise, then re-normalled so the lighting follows the new
/// surface rather than the sphere it came from.
///
/// The displacement is deliberately applied to the *unit* direction and scaled by radius
/// at the end, so a 9 m boulder and a 1.5 m stone drawn from the same seed are the same
/// shape at two sizes — the noise doesn't get finer as rocks get bigger.
fn rock_mesh(radius: f32, seed: u32) -> Mesh {
    let mut mesh = Sphere::new(1.0)
        .mesh()
        .ico(ROCK_SUBDIVISIONS)
        // The only failure is asking for more subdivisions than the generator supports;
        // ours is a constant well inside the limit, so the fallback is unreachable — but a
        // low-poly rock beats a panicked client.
        .unwrap_or_else(|_| Sphere::new(1.0).mesh().ico(1).unwrap_or_else(|_| Sphere::new(1.0).mesh().uv(8, 6)));

    let Some(VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute_mut(Mesh::ATTRIBUTE_POSITION)
    else {
        return mesh;
    };
    for position in positions.iter_mut() {
        let dir = Vec3::from_array(*position).normalize_or(Vec3::Y);
        let mut scale = 1.0;
        for (frequency, amplitude) in OCTAVES {
            // Centred on zero, so the octaves carve as much as they pile on and the rock
            // keeps roughly the radius its collider claims.
            scale += (value_noise(dir * frequency, seed) - 0.5) * 2.0 * amplitude;
        }
        *position = (dir * scale * radius).to_array();
    }
    // The sphere's normals are its own radii, which after displacement point at the shape
    // the rock *used* to be — every facet would light as though it were still round.
    mesh.compute_normals();
    mesh.asset_usage = RenderAssetUsages::RENDER_WORLD;
    // Nothing samples a texture on a rock, and the displaced sphere's UVs are meaningless
    // anyway; dropping them keeps the vertex buffer to what is actually drawn.
    mesh.remove_attribute(Mesh::ATTRIBUTE_UV_0);
    mesh
}

/// Dull and not remotely metallic — the rock reads by its shape and its shadowing. The
/// seed only nudges brightness and how much iron-red is in it, so a field varies without
/// any one of them looking like a different material.
///
/// **Pale, not dark.** Real rock is dark (basalt's albedo is under 0.1) and the first cut
/// used those numbers — which produced boulders that were, correctly, invisible: from a few
/// kilometres up the sky is black, the sun in this scene is a faint diffuse ember, and a
/// 0.06-albedo sphere against black is nothing at all. Measured on a real ascent, a field
/// running at full spawn rate was not visible in a single frame. A hazard the pilot cannot
/// see is not difficulty. So these are the pale, dusty end of the rubble — bright enough to
/// hold an edge against both the ash haze near the pad and the black above it.
fn rock_material(seed: u32) -> StandardMaterial {
    let mut s = seed as u64 ^ 0x0A57E401D;
    let brightness = 0.34 + next_unit(&mut s) * 0.22;
    let rust = next_unit(&mut s) * 0.35;
    StandardMaterial {
        base_color: Color::srgb(
            brightness * (1.0 + rust),
            brightness * (1.0 - rust * 0.25),
            brightness * (1.0 - rust * 0.5),
        ),
        // A whisper of self-illumination, well under the diffuse term, so a rock on the
        // sun's far side is still a shape rather than a hole in the starfield. It is the
        // one non-physical touch here and it is deliberate: the alternative is a black
        // sphere on a black sky, which reads as nothing until it hits you.
        emissive: LinearRgba::rgb(0.035, 0.030, 0.026),
        perceptual_roughness: 0.92 + next_unit(&mut s) * 0.08,
        metallic: 0.0,
        ..default()
    }
}

/// Smooth 3D value noise in `[0, 1)`: hash the eight corners of the lattice cell `p` falls
/// in and blend them with a smoothstep. Enough for carving a boulder, and — unlike a
/// `rand` draw — a pure function of position and seed, so every client carves the same one.
fn value_noise(p: Vec3, seed: u32) -> f32 {
    let cell = p.floor();
    let f = p - cell;
    // Smoothstep the interpolant so the lattice grid doesn't show up as creases.
    let t = f * f * (3.0 - 2.0 * f);
    let (x, y, z) = (cell.x as i32, cell.y as i32, cell.z as i32);
    let corner = |dx, dy, dz| lattice(x + dx, y + dy, z + dz, seed);
    let lerp3 = |a: f32, b: f32, w: f32| a + (b - a) * w;
    let x00 = lerp3(corner(0, 0, 0), corner(1, 0, 0), t.x);
    let x10 = lerp3(corner(0, 1, 0), corner(1, 1, 0), t.x);
    let x01 = lerp3(corner(0, 0, 1), corner(1, 0, 1), t.x);
    let x11 = lerp3(corner(0, 1, 1), corner(1, 1, 1), t.x);
    lerp3(lerp3(x00, x10, t.y), lerp3(x01, x11, t.y), t.z)
}

/// One lattice corner's value in `[0, 1)`. The odd multipliers are the usual large odd
/// constants used to spread integer coordinates across the whole word before mixing, so
/// neighbouring cells don't land on neighbouring hashes.
fn lattice(x: i32, y: i32, z: i32, seed: u32) -> f32 {
    let mut state = (x as i64 as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (z as i64 as u64).wrapping_mul(0x1656_67B1_9E37_79F9)
        ^ seed as u64;
    next_unit(&mut state)
}

/// One shared-`splitmix64` draw folded to a uniform f32 in `[0, 1)` — the same helper the
/// metal finish uses, and splitmix for the same reason: it is the seeded generator both
/// peers agree on, where `rand` is not.
fn next_unit(state: &mut u64) -> f32 {
    (splitmix64(state) >> 40) as f32 / (1u64 << 24) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two clients carving the same seed must get the same rock, and two seeds must not.
    #[test]
    fn a_rock_is_determined_by_its_seed() {
        let a = rock_mesh(4.0, 12345);
        let b = rock_mesh(4.0, 12345);
        let c = rock_mesh(4.0, 12346);
        let verts = |m: &Mesh| match m.attribute(Mesh::ATTRIBUTE_POSITION) {
            Some(VertexAttributeValues::Float32x3(v)) => v.clone(),
            _ => panic!("rock mesh lost its positions"),
        };
        assert_eq!(verts(&a), verts(&b), "same seed carved two different rocks");
        assert_ne!(verts(&a), verts(&c), "two seeds carved the same rock");
    }

    /// The carving must stay inside the collider it is drawn for: a vertex pushed far past
    /// the sphere would be visibly struck by nothing, and one pushed through the centre
    /// would invert the surface.
    #[test]
    fn the_carved_rock_stays_around_its_radius() {
        let total: f32 = OCTAVES.iter().map(|(_, amplitude)| amplitude).sum();
        assert!(total < 0.5, "octaves can displace a vertex through the rock's centre");
        for seed in 0..32u32 {
            let radius = 6.0;
            let mesh = rock_mesh(radius, seed);
            let Some(VertexAttributeValues::Float32x3(positions)) =
                mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            else {
                panic!("rock mesh lost its positions");
            };
            for position in positions {
                let r = Vec3::from_array(*position).length();
                assert!(
                    r > radius * (1.0 - total) - 1e-3 && r < radius * (1.0 + total) + 1e-3,
                    "seed {seed}: vertex at {r} m is outside the {radius} m rock's band"
                );
            }
        }
    }
}
