//! The flight trajectory line: a yellow dotted path through the sky showing where the
//! launched assembly **has been** (the flown trail) and where the autopilot **plans to
//! take it** (the ascent plan re-propagated from the live state) — look up at liftoff
//! and the dots bend over ahead of you where the gravity turn will lean.
//!
//! Both halves live in the **true planet frame** (the trail in f64 — at Mm-scale
//! altitudes f32 steps by whole metres) and are folded back to room-local coordinates
//! through the room's visual floating-origin frame each frame, so a rebase slides the
//! line rigidly with the rest of the world. The future half is the same point-mass
//! [`propagate`] the pitchover optimizer flies (drag included), re-run from the current
//! state every [`REPLAN_SECS`] — so the line converges on what the vehicle actually
//! does rather than freezing the launch-day plan; after the escape cutoff it shows the
//! engines-off ballistic coast instead.
//!
//! Rendering is one rebuilt-per-frame mesh of small octahedral dots. Dot **spacing and
//! size scale with distance from the camera**, so the line reads as an evenly dotted
//! screen-space path from the pad to a plan-end tens of kilometres up (fixed-size dots
//! would vanish at distance or merge nearby), and the dot count stays log-bounded. The
//! material is unlit with fog disabled: the trajectory is an instrument overlay, and it
//! must stay legible **through** the smog it is about to climb out of — while still
//! depth-tested, so the planet properly occludes the far side of an orbit-scale arc.

use bad_spaceship_shared::guidance::{
    propagate, GROUND_RADIUS, OPTIMIZER_DT, OPTIMIZER_STEPS,
};
use bad_spaceship_shared::map::{drag_force, gravity_at, radial_altitude, PLANET_CENTER};
use bevy::{
    asset::RenderAssetUsages,
    camera::visibility::NoFrustumCulling,
    math::DVec3,
    mesh::Indices,
    prelude::*,
    render::render_resource::PrimitiveTopology,
};

use crate::launch::Autopilot;
use crate::net::ClientRoomFrame;

pub struct TrajectoryPlugin;

impl Plugin for TrajectoryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FlightPath>()
            .add_systems(Startup, spawn_trajectory_line)
            .add_systems(Update, (update_flight_path, draw_flight_path).chain());
    }
}

/// Seconds between re-propagations of the future half. The point-mass sim is cheap
/// (it's the optimizer's inner loop) but not per-frame cheap.
const REPLAN_SECS: f32 = 0.5;

/// Sample the burn preview every N integrator steps (× [`OPTIMIZER_DT`] = 0.5 s of
/// flight per sample) — dense enough that the dot walk sees a smooth curve.
const PREVIEW_SAMPLE_EVERY: usize = 5;

/// How far ahead the engines-off coast preview integrates (s), and its step/sampling.
const COAST_PREVIEW_SECS: f32 = 240.0;
const COAST_DT: f32 = 0.25;
const COAST_SAMPLE_EVERY: usize = 4;

/// Trail recording: capacity cap (halved by thinning when full) and the minimum spacing
/// between recorded points — 1% of altitude, floored, so the record stays dense near
/// the pad and sparse (but never empty) across a thousand-kilometre coast.
const MAX_TRAIL: usize = 4096;
const TRAIL_MIN_STEP: f64 = 2.0;

/// Dot layout: spacing and radius per metre of camera distance (an even screen-space
/// rhythm — ~2 dots per degree, each ~a third of a degree wide), with floors so the
/// dots right at the pad stay visible, and a hard count cap as a safety net.
const DOT_SPACING_PER_M: f32 = 0.035;
const DOT_RADIUS_PER_M: f32 = 0.006;
const MIN_SPACING: f32 = 1.5;
const MIN_RADIUS: f32 = 0.12;
const MAX_DOTS: usize = 4000;

/// The recorded + predicted flight path, in true planet-frame coordinates.
#[derive(Resource, Default)]
pub struct FlightPath {
    /// Where the assembly has been this launch (oldest first).
    trail: Vec<DVec3>,
    /// Where the plan takes it from here (re-propagated every [`REPLAN_SECS`]).
    future: Vec<DVec3>,
    /// HUD readout of the current burn preview; `None` while coasting (engines cut).
    pub plan: Option<PlanReadout>,
}

/// What the burn preview says about the remaining burn, for the flight HUD.
pub struct PlanReadout {
    /// Whether the previewed burn reaches a secured escape (a healthy plan always
    /// does; `false` warns the trajectory re-enters or the step budget ran out).
    pub escapes: bool,
    /// Predicted seconds until the escape cutoff shuts the engines off.
    pub eta_secs: f32,
    /// Predicted altitude (m) at the cutoff point.
    pub cutoff_alt: f32,
}

/// Marker for the one trajectory-line mesh entity. Spawned **without** a `Mesh3d`:
/// the draw system swaps in a freshly-allocated mesh asset per rebuild (replacing the
/// handle drops the old asset). Registering an attribute-less placeholder mesh — or
/// resizing one asset's buffers every frame — trips bevy_render's slab allocator on
/// WebGL2 ("Use-after-free: attempted to copy element data for an unallocated key")
/// and wedges the whole renderer the first frame the line goes visible.
#[derive(Component)]
struct TrajectoryLine;

fn spawn_trajectory_line(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        TrajectoryLine,
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.85, 0.2),
            unlit: true,
            // An instrument overlay: must read through the smog (see the module doc).
            fog_enabled: false,
            ..default()
        })),
        // The mesh is rebuilt around the camera every frame; its baked AABB means nothing.
        NoFrustumCulling,
        Transform::IDENTITY,
        Visibility::Hidden,
        Name::new("Trajectory line"),
    ));
}

/// Record the flown trail and re-propagate the future half from the autopilot's live
/// state (see the module doc for the trail/preview policy).
fn update_flight_path(
    time: Res<Time>,
    autopilot: Res<Autopilot>,
    mut path: ResMut<FlightPath>,
    mut replan_in: Local<f32>,
) {
    let Some(snap) = autopilot.0.as_ref() else {
        // Launch over (or broke up): clear everything so the next launch starts fresh.
        if !path.trail.is_empty() || !path.future.is_empty() || path.plan.is_some() {
            *path = FlightPath::default();
        }
        *replan_in = 0.0;
        return;
    };

    // Trail: append when we've moved a step past the last record; thin by halving when
    // full (doubles the record's spacing everywhere — invisible under the
    // distance-scaled dot walk).
    let step = (radial_altitude(snap.true_pos.as_vec3()) as f64 * 0.01).max(TRAIL_MIN_STEP);
    if path.trail.last().is_none_or(|last| last.distance(snap.true_pos) >= step) {
        path.trail.push(snap.true_pos);
        if path.trail.len() > MAX_TRAIL {
            let mut keep = false;
            path.trail.retain(|_| {
                keep = !keep;
                keep
            });
        }
    }

    *replan_in -= time.delta_secs();
    if *replan_in > 0.0 {
        return;
    }
    *replan_in = REPLAN_SECS;

    let pos = snap.true_pos.as_vec3();
    if snap.throttle > 0.0 {
        // Burning: fly the same drag-aware point-mass law the optimizer ranked, from
        // the live state — the preview IS the plan, continuously re-anchored.
        let preview = propagate(
            pos,
            snap.true_vel,
            snap.vehicle,
            snap.pitchover,
            OPTIMIZER_DT,
            OPTIMIZER_STEPS,
            PREVIEW_SAMPLE_EVERY,
            GROUND_RADIUS,
        );
        path.plan = Some(PlanReadout {
            escapes: preview.escaped,
            // The path is one sample per PREVIEW_SAMPLE_EVERY steps, so its length is
            // the flight time to the end point (± one sample interval — HUD-grade).
            eta_secs: preview.path.len().saturating_sub(1) as f32
                * PREVIEW_SAMPLE_EVERY as f32
                * OPTIMIZER_DT,
            cutoff_alt: preview.path.last().map_or(0.0, |p| radial_altitude(*p)),
        });
        path.future = preview.path.iter().map(|p| p.as_dvec3()).collect();
    } else {
        // Engines cut: the plan ahead is the ballistic coast (gravity + residual drag).
        path.future = coast_path(pos, snap.true_vel, snap.vehicle.mass);
        path.plan = None;
    }
}

/// Forward-integrate an engines-off coast (gravity + drag only) for the preview —
/// the throttle-zero counterpart of [`propagate`], which always burns.
fn coast_path(mut pos: Vec3, mut vel: Vec3, mass: f32) -> Vec<DVec3> {
    let steps = (COAST_PREVIEW_SECS / COAST_DT) as usize;
    let mut out = Vec::with_capacity(steps / COAST_SAMPLE_EVERY + 2);
    out.push(pos.as_dvec3());
    for step in 1..=steps {
        let accel = gravity_at(pos) + drag_force(pos, vel) / mass;
        vel += accel * COAST_DT;
        pos += vel * COAST_DT;
        if (pos - PLANET_CENTER).length() < GROUND_RADIUS {
            out.push(pos.as_dvec3()); // show the impact point, then stop
            break;
        }
        if step % COAST_SAMPLE_EVERY == 0 {
            out.push(pos.as_dvec3());
        }
    }
    out
}

/// Rebuild the dotted-line mesh: fold the true-frame path into room-local coordinates,
/// walk it emitting camera-distance-scaled dots, and write one octahedron per dot.
fn draw_flight_path(
    mut commands: Commands,
    path: Res<FlightPath>,
    autopilot: Res<Autopilot>,
    frame: Option<Res<ClientRoomFrame>>,
    camera: Query<&GlobalTransform, With<Camera3d>>,
    mut line: Query<(Entity, &mut Visibility), With<TrajectoryLine>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let Ok((entity, mut visibility)) = line.single_mut() else {
        return;
    };
    let Ok(camera) = camera.single() else {
        return;
    };
    // trail → current position → plan, one polyline in local coordinates. The visual
    // frame mirror keeps `true − offset` continuous through a rebase snap.
    let offset = frame.map(|f| f.offset).unwrap_or(DVec3::ZERO);
    let mut points: Vec<Vec3> = Vec::with_capacity(path.trail.len() + path.future.len() + 1);
    points.extend(path.trail.iter().map(|p| (p - offset).as_vec3()));
    if let Some(snap) = autopilot.0.as_ref() {
        points.push((snap.true_pos - offset).as_vec3());
    }
    // `future[0]` is the propagation start — the current position again; skip it.
    points.extend(path.future.iter().skip(1).map(|p| (p - offset).as_vec3()));

    let cam = camera.translation();
    let mut dots: Vec<(Vec3, f32)> = Vec::new();
    // Arc-length walk: `carry` is the distance still to go before the next dot,
    // carried across segment boundaries so corners don't double-dot.
    let mut carry = 0.0f32;
    'walk: for pair in points.windows(2) {
        let seg = pair[1] - pair[0];
        let len = seg.length();
        if !len.is_finite() || len <= 1e-4 {
            continue; // degenerate or non-finite segment
        }
        let dir = seg / len;
        let mut t = carry;
        while t < len {
            let p = pair[0] + dir * t;
            let dist = p.distance(cam);
            dots.push((p, (dist * DOT_RADIUS_PER_M).max(MIN_RADIUS)));
            if dots.len() >= MAX_DOTS {
                break 'walk;
            }
            t += (dist * DOT_SPACING_PER_M).max(MIN_SPACING);
        }
        carry = t - len;
    }

    if dots.is_empty() {
        *visibility = Visibility::Hidden;
        return;
    }
    *visibility = Visibility::Visible;
    // A fresh asset per rebuild (see [`TrajectoryLine`] for why not in-place mutation);
    // replacing the `Mesh3d` handle drops the previous frame's mesh.
    commands.entity(entity).insert(Mesh3d(meshes.add(dot_mesh(&dots))));
}

/// Build a mesh with one small octahedron per `(center, radius)` dot — the cheapest
/// solid that reads as a round point from any angle (6 vertices, 8 faces).
fn dot_mesh(dots: &[(Vec3, f32)]) -> Mesh {
    const AXES: [Vec3; 6] = [
        Vec3::X,
        Vec3::NEG_X,
        Vec3::Y,
        Vec3::NEG_Y,
        Vec3::Z,
        Vec3::NEG_Z,
    ];
    // Outward-wound (CCW from outside) faces over the six axis vertices above.
    const FACES: [[u32; 3]; 8] = [
        [0, 2, 4],
        [4, 2, 1],
        [1, 2, 5],
        [5, 2, 0],
        [0, 4, 3],
        [4, 1, 3],
        [1, 5, 3],
        [5, 0, 3],
    ];
    let mut positions = Vec::with_capacity(dots.len() * 6);
    let mut normals = Vec::with_capacity(dots.len() * 6);
    let mut indices = Vec::with_capacity(dots.len() * 24);
    for (i, (center, radius)) in dots.iter().enumerate() {
        let base = (i * 6) as u32;
        for axis in AXES {
            positions.push((*center + axis * *radius).to_array());
            normals.push(axis.to_array());
        }
        indices.extend(FACES.iter().flat_map(|f| f.map(|v| base + v)));
    }
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_indices(Indices::U32(indices))
}
