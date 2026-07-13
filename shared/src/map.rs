use avian3d::dynamics::rigid_body::forces::ForcesItem;
use avian3d::prelude::{Collider, RigidBody, WriteRigidBodyForces};
use bevy::prelude::*;

use crate::Grass;
pub struct MapPlugin;

impl Plugin for MapPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_map);
    }
}

pub const PLATFORM_WIDTH_M: f32 = 50.0; // meters
pub const PLATFORM_THICKNESS_M: f32 = 3.0; // meters

// ---------------------------------------------------------------------------
// Mini planet
// ---------------------------------------------------------------------------
//
// The 50 m grass platform is now the top of a mesa: cliffs drop from its edge
// down to a giant low-poly magma sphere `PLANET_DROP` metres below. The planet
// is a *client-side visual* (rendered with the magma material) plus a couple of
// server-side y-thresholds — it has no 30 km collider. Instead:
//   * a player who walks off the cliff falls past `PLANET_SURFACE_Y` and is
//     respawned by the normal fall path (the platform is the only thing you can
//     stand on before blastoff);
//   * a built assembly whose parts drop below `ASSEMBLY_CRASH_Y` (well below the
//     platform but above the loose-part cull line) has toppled off — a crash,
//     which resets the room.

/// Metres the planet's top surface sits below the platform base (world y = 0).
pub const PLANET_DROP: f32 = 20.0;
/// World-space y of the planet's top surface (directly below the platform).
pub const PLANET_SURFACE_Y: f32 = -PLANET_DROP;
/// 30 km diameter — a proper little world curving away to a near horizon.
pub const PLANET_RADIUS: f32 = 15_000.0;
/// World-space y of the sphere's centre (surface minus radius).
pub const PLANET_CENTER_Y: f32 = PLANET_SURFACE_Y - PLANET_RADIUS;
/// The planet centre in true world coordinates — the single source for every module that
/// measures radial distance (gravity, guidance, orbital energy).
pub const PLANET_CENTER: Vec3 = Vec3::new(0.0, PLANET_CENTER_Y, 0.0);
/// Height a grounded avatar respawns at once it falls off the cliffs — 2 m above the
/// planet surface, so it never visibly clips into the magma. Shared by the server
/// (`respawn_fallen_avatars`) and single-player (`player::despawn`) so the two respawn
/// heights stay in lockstep.
pub const PLANET_RESPAWN_Y: f32 = PLANET_SURFACE_Y + 2.0;

// ---------------------------------------------------------------------------
// Planet gravity
// ---------------------------------------------------------------------------
//
// Gravity points at the planet's centre and weakens with altitude by Newton's
// inverse-square law, `g(r) = μ/r²`. It is applied as a per-body *correction* on
// top of Avian's unchanged uniform `Gravity` (0, −9.81, 0): the correction is
// `gravity_at(true_pos) − uniform`, which is ~zero at the pad (so building and the
// thrust/hold feed-forward are untouched) and grows as the assembly climbs — a real
// rocket keeps its calibrated (constant) thrust while its weight falls away, so it
// accelerates ever harder with altitude. `true_pos` is the body's TRUE world position:
// the floating-origin frame offset folded back in (`RoomFrames`/`ClientRoomFrame`), so
// `r` is the real distance from the centre even while the co-moving frame keeps the
// body near the local origin.

/// Surface gravity magnitude (m/s²) — the old uniform field's strength, preserved: the
/// planet's "mass" (`GRAVITY_MU`) is tuned so gravity at the platform play level equals
/// this exactly.
pub const SURFACE_GRAVITY: f32 = 9.81;
/// Distance from the planet centre to the platform play surface (world y = 0) — the
/// radius at which gravity equals [`SURFACE_GRAVITY`]. Equals `PLANET_RADIUS +
/// PLANET_DROP` (= `-PLANET_CENTER_Y`): the platform sits `PLANET_DROP` above the 15 km
/// sphere.
pub const GRAVITY_REF_RADIUS: f32 = -PLANET_CENTER_Y;
/// Standard gravitational parameter μ = g₀·R² (m³/s²) — the planet's mass expressed the
/// way the inverse-square law needs it, tuned so `gravity_at` yields [`SURFACE_GRAVITY`]
/// at [`GRAVITY_REF_RADIUS`].
pub const GRAVITY_MU: f32 = SURFACE_GRAVITY * GRAVITY_REF_RADIUS * GRAVITY_REF_RADIUS;

/// The planet's gravitational acceleration (m/s²) at a body's TRUE world position
/// (floating-origin offset already folded in). Always points at the sphere centre
/// `(0, PLANET_CENTER_Y, 0)` with magnitude `μ/r²`, so it is [`SURFACE_GRAVITY`] straight
/// down at the pad and falls off with altitude. Shared by every world (single-player,
/// server, predicted client) so the field they simulate is identical.
pub fn gravity_at(true_pos: Vec3) -> Vec3 {
    let to_center = PLANET_CENTER - true_pos;
    let r2 = to_center.length_squared();
    // Guard the r→0 singularity: the surface is 15 km out, so a body never nears the
    // centre — this only stops a NaN if one somehow tunnels there.
    if r2 < 1.0 {
        return Vec3::ZERO;
    }
    // to_center/|to_center| · μ/r² = μ·to_center / r³.
    to_center * (GRAVITY_MU / (r2 * r2.sqrt()))
}

/// Apply the planet-gravity *correction* to one body: the difference between the radial
/// field at `true_pos` ([`gravity_at`]) and Avian's uniform `gravity`, as a
/// mass-independent acceleration through `Forces`. Called from all three worlds
/// (single-player, predicted client, server) so the field they simulate is identical —
/// the same shared-helper discipline `apply_hold_spring` uses for the held-part spring,
/// and it single-sources the "field minus the uniform reference" contract so the sites
/// can't drift.
pub fn apply_gravity_correction(forces: &mut ForcesItem, true_pos: Vec3, gravity: Vec3) {
    forces.apply_linear_acceleration(gravity_at(true_pos) - gravity);
}

/// The Avian collision-layer bit the ground sits on: bit 0 (value 1), Avian's
/// default membership for a collider with no explicit `CollisionLayers` — like the
/// bowl spawned below. The multiplayer collision scheme reserves it for the ground
/// so rooms never use it: room parts/avatars (`server::net`) put their `room.bit`
/// in membership and add `GROUND_LAYER` to their *filter* (`room.bit | GROUND_LAYER`)
/// so they still land on the ground while staying isolated from other rooms; a
/// freshly-spawned, not-yet-roomed avatar (`build_server_avatar`) uses it as both
/// membership and filter to collide with the ground *only*.
pub const GROUND_LAYER: u32 = 1;

/// Build the ground bowl's trimesh collider (a cosine cross-section bowl with a flat
/// lip). `pub` so the joint-thinning tuning tests weld against the EXACT trimesh the
/// live ground uses, rather than a duplicated approximation that could silently drift.
pub fn bowl_collider() -> Collider {
    let mut vertices: Vec<Vec3> = Vec::new();
    let mut indices: Vec<[u32; 3]> = Vec::new();
    let segments = 16;
    let bowl_size = Vec3::new(PLATFORM_WIDTH_M, PLATFORM_THICKNESS_M, PLATFORM_WIDTH_M);
    for ix in 0..=segments {
        for iz in 0..=segments {
            // Map x and y into range [-1.0, 1.0];
            let shifted_z = (iz as f32 / segments as f32 - 0.5) * 2.0;
            let shifted_x = (ix as f32 / segments as f32 - 0.5) * 2.0;
            // Clamp radius at 1.0 or lower so the bowl has a flat lip near the corners.
            let clamped_radius = (shifted_z.powi(2) + shifted_x.powi(2)).sqrt().min(1.0);
            let x = shifted_x * bowl_size.x / 2.0;
            let z = shifted_z * bowl_size.z / 2.0;
            let y =
                ((clamped_radius - 0.5) * std::f32::consts::TAU / 2.0).sin() * bowl_size.y / 2.0;
            vertices.push([x, y, z].into());
        }
    }
    for ix in 0..segments {
        // Start of the two relevant rows of vertices.
        let row0 = ix * (segments + 1);
        let row1 = (ix + 1) * (segments + 1);

        for iz in 0..segments {
            // Two triangles making up a not-very-flat quad for each segment of the bowl.
            indices.push([row0 + iz + 0, row0 + iz + 1, row1 + iz + 0]);
            indices.push([row1 + iz + 0, row0 + iz + 1, row1 + iz + 1]);
        }
    }
    // Avian's fallible trimesh constructor is `try_trimesh`; the bowl mesh is always
    // valid (non-empty, matching index count), so unwrap it.
    Collider::try_trimesh(vertices, indices).expect("valid bowl trimesh")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gravity_is_surface_strength_straight_down_at_the_pad() {
        // A body on the platform (world y = 0) sits at the reference radius, so gravity
        // is exactly SURFACE_GRAVITY pointing straight down — the old uniform field,
        // preserved. (Slightly off-axis, still ~straight down: the 15 km sphere barely
        // curves over the 50 m pad.)
        let g = gravity_at(Vec3::ZERO);
        assert!((g.x).abs() < 1e-3 && (g.z).abs() < 1e-3, "level at the pad: {g:?}");
        assert!((g.y + SURFACE_GRAVITY).abs() < 1e-2, "surface strength: {g:?}");
    }

    #[test]
    fn gravity_weakens_with_altitude_by_inverse_square() {
        // At one reference radius of altitude the distance from the centre doubles, so
        // gravity should be a quarter of surface (inverse-square).
        let up = -PLANET_CENTER_Y; // one ref-radius above the pad → r = 2·R_ref
        let g = gravity_at(Vec3::new(0.0, up, 0.0));
        assert!((g.y + SURFACE_GRAVITY / 4.0).abs() < 1e-2, "quarter g at 2R: {g:?}");
        // Monotone falloff: higher is always weaker.
        let higher = gravity_at(Vec3::new(0.0, 2.0 * up, 0.0));
        assert!(higher.length() < g.length(), "weaker higher up");
    }

    // A real Avian step, to prove the *correction* approach the client/server systems
    // use — keep Avian's uniform 9.81 and apply `gravity_at − uniform` through `Forces`
    // — yields the right NET gravity: 9.81 at the pad, the reduced inverse-square value
    // at altitude (a sign error would show here as too-strong or upward net gravity).
    #[test]
    fn correction_force_yields_reduced_net_gravity_at_altitude() {
        use avian3d::prelude::{Forces, Gravity, LinearVelocity, Position, RigidBody};
        use avian3d::PhysicsPlugins;
        use bevy::time::TimeUpdateStrategy;
        use core::time::Duration;

        const DT: f32 = 1.0 / 60.0;

        #[derive(Component)]
        struct Falls;

        // The exact per-body correction the real gravity systems apply (offset = 0 here) —
        // through the shared helper, so this test exercises the real path.
        fn correct(gravity: Res<Gravity>, mut bodies: Query<(&Position, Forces), With<Falls>>) {
            for (position, mut forces) in &mut bodies {
                apply_gravity_correction(&mut forces, position.0, gravity.0);
            }
        }

        // Drop a body from `y` and return its steady-state downward acceleration
        // (m/s², positive = downward), measured as the velocity gained between two
        // post-warmup samples so the integrator's start-up transient drops out.
        fn fall_accel(y: f32) -> f32 {
            const WARMUP: usize = 5;
            const MEASURE: usize = 30;
            let mut app = App::new();
            app.add_plugins((MinimalPlugins, TransformPlugin, PhysicsPlugins::default()));
            app.insert_resource(Gravity(Vec3::NEG_Y * SURFACE_GRAVITY));
            app.insert_resource(Time::<Fixed>::from_duration(Duration::from_secs_f32(DT)));
            app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(DT)));
            app.add_systems(FixedUpdate, correct);
            let body = app
                .world_mut()
                .spawn((
                    RigidBody::Dynamic,
                    Collider::sphere(0.5),
                    Position(Vec3::new(0.0, y, 0.0)),
                    LinearVelocity::default(),
                    Falls,
                ))
                .id();
            app.finish();
            for _ in 0..WARMUP {
                app.update();
            }
            let v0 = app.world().get::<LinearVelocity>(body).unwrap().0.y;
            for _ in 0..MEASURE {
                app.update();
            }
            let v1 = app.world().get::<LinearVelocity>(body).unwrap().0.y;
            -(v1 - v0) / (MEASURE as f32 * DT)
        }

        // At the pad the net field is the unchanged surface gravity.
        let pad = fall_accel(0.0);
        assert!((pad - SURFACE_GRAVITY).abs() < 0.02, "pad net gravity {pad}");
        // One ref-radius up (r = 2·R_ref) the net field is a quarter — the correction
        // has cancelled three-quarters of the uniform pull, not added to it.
        let high = fall_accel(-PLANET_CENTER_Y);
        assert!((high - SURFACE_GRAVITY / 4.0).abs() < 0.02, "altitude net gravity {high}");
        assert!(high < pad, "gravity weaker up high");
    }

    #[test]
    fn gravity_points_at_the_centre_off_axis() {
        // A body displaced sideways feels gravity aimed at the sphere centre, not
        // straight down — a horizontal pull back toward the axis.
        let p = Vec3::new(5000.0, 0.0, 0.0);
        let g = gravity_at(p);
        let to_center = (Vec3::new(0.0, PLANET_CENTER_Y, 0.0) - p).normalize();
        assert!(g.normalize().dot(to_center) > 0.999, "aimed at centre: {g:?}");
        assert!(g.x < 0.0, "pulled back toward the axis: {g:?}");
    }
}

fn spawn_map(mut commands: Commands) {
    commands
        .spawn_empty()
        // Avian renamed rapier's `RigidBody::Fixed` to `Static`.
        .insert(RigidBody::Static)
        // Bevy 0.15: `Transform` now requires `GlobalTransform`, so inserting the
        // bare component replaces the old `TransformBundle`. (Avian collides all
        // collider pairs by default, so rapier's `ActiveCollisionTypes` opt-in is gone.)
        .insert(Transform::from_xyz(0.0, 0.0, 0.0))
        // The client parents the mini-planet's visuals (sphere + cliffs) under this
        // entity so they ride the floating-origin frame. Give it `Visibility` from
        // the start so those child meshes never attach to a parent that lacks
        // `InheritedVisibility` (a first-frame B0004 warning otherwise). Inert on the
        // headless server.
        .insert(Visibility::default())
        .insert(bowl_collider())
        .insert(Grass);
}
