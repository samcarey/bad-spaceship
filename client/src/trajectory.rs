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
//! **Stability.** Two things keep the line from thrashing. (1) The recorded **trail is
//! continuity-gated**: a candidate point is dropped when its jump from the previous frame
//! is far larger than speed·dt allows — a transient rollback replay or a one-frame
//! floating-origin `offset`↔`com` desync would otherwise bake a permanent spike into the
//! past that "never happened". (2) The **future eases**: each re-propagation lands in a
//! `future_target`, and the drawn `future_display` approaches it exponentially
//! ([`FUTURE_EASE_RATE`]) rather than snapping, so a re-plan slides the plan line over
//! instead of jumping. The eased shape is stored as offsets from the live position and
//! re-anchored to it each frame, so it stays glued to the vehicle as it settles.
//!
//! Rendering is one rebuilt-per-frame **3D tube** — a thin extruded pipe following the
//! path, its radius scaling with camera distance so the line holds a roughly constant
//! on-screen thickness from the pad to a plan-end tens of kilometres up. (A camera-facing
//! flat ribbon was tried first and rejected: a billboarded strip collapses to a
//! flickering 2D sliver when viewed edge-on — looking along the line — and creases at
//! bends. A tube reads identically from every angle.) The frame is carried along the path
//! by parallel transport so the tube never kinks. The material is unlit with fog
//! disabled: the trajectory is an instrument overlay, and it must stay legible **through**
//! the smog it is about to climb out of — while still depth-tested, so the planet properly
//! occludes the far side of an orbit-scale arc.

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
use bevy_egui::PrimaryEguiContext;

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

/// Every re-propagation is resampled to this fixed point count so the drawn shape can
/// ease toward it **point-for-point** (each index is the same look-ahead time in both the
/// old and the new plan). Plenty of resolution for a thin line following a smooth arc.
const FUTURE_SAMPLES: usize = 96;

/// Exponential rate the drawn future approaches a freshly re-planned one (1/s) — a ~0.3 s
/// time constant, so a re-plan slides over in under a second instead of snapping. Frame-
/// rate independent (`1 − e^(−rate·dt)`).
const FUTURE_EASE_RATE: f32 = 3.0;

/// A trail candidate is rejected as a glitch when its jump from the previous frame exceeds
/// `speed·dt·SLACK + FLOOR` — generous headroom over real motion (which is exactly
/// speed·dt) that still catches km-scale rollback/rebase teleports.
const TRAIL_JUMP_SLACK: f64 = 5.0;
const TRAIL_JUMP_FLOOR: f64 = 100.0;

/// How far ahead the engines-off coast preview integrates (s), and its step/sampling.
const COAST_PREVIEW_SECS: f32 = 240.0;
const COAST_DT: f32 = 0.25;
const COAST_SAMPLE_EVERY: usize = 4;

/// Trail recording: capacity cap (halved by thinning when full) and the minimum spacing
/// between recorded points — 1% of altitude, floored, so the record stays dense near
/// the pad and sparse (but never empty) across a thousand-kilometre coast.
const MAX_TRAIL: usize = 4096;
const TRAIL_MIN_STEP: f64 = 2.0;

/// Line thickness: tube radius as a fraction of the camera distance (so it holds a
/// roughly constant on-screen width from the pad to a plan-end tens of km up), floored so
/// the near end never collapses to nothing.
const LINE_RADIUS_PER_M: f32 = 0.00075;
const MIN_RADIUS: f32 = 0.0125;
/// Cross-section sides of the tube. Few: it's a thin line, so it only needs to read as
/// round-ish from any angle, not be smooth.
const TUBE_SIDES: usize = 5;

/// The recorded + predicted flight path, in true planet-frame coordinates.
#[derive(Resource, Default)]
pub struct FlightPath {
    /// Where the assembly has been this launch (oldest first).
    trail: Vec<DVec3>,
    /// The live vehicle position (true frame) after the continuity gate — the junction
    /// where the trail meets the future. Held frozen for the odd glitch frame so a
    /// transient position spike can't jerk the whole line.
    current: DVec3,
    /// The latest re-propagated plan, as offsets from [`current`](Self::current),
    /// resampled to [`FUTURE_SAMPLES`] — the shape the drawn line eases toward.
    future_target: Vec<DVec3>,
    /// The eased shape actually drawn: approaches [`future_target`](Self::future_target)
    /// at [`FUTURE_EASE_RATE`], re-anchored to [`current`](Self::current) in the draw.
    future_display: Vec<DVec3>,
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
            // Neon (electric highlighter) yellow — the scene has no bloom, so an unlit
            // fully-saturated bright yellow is what reads as "neon".
            base_color: Color::srgb(0.95, 1.0, 0.05),
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
    // Previous frame's true position, for the trail continuity gate (below).
    mut prev_pos: Local<Option<DVec3>>,
) {
    let Some(snap) = autopilot.0.as_ref() else {
        // Launch over (or broke up): clear everything so the next launch starts fresh.
        if !path.trail.is_empty() || !path.future_display.is_empty() || path.plan.is_some() {
            *path = FlightPath::default();
        }
        *replan_in = 0.0;
        *prev_pos = None;
        return;
    };
    let dt = time.delta_secs();

    // Trail (past): only advance on a frame whose motion is physically plausible. A real
    // step is speed·dt; a jump wildly past that is a glitch (a rollback replay, or a
    // one-frame floating-origin `offset`↔`com` desync in MP) that would otherwise bake a
    // permanent spike into the recorded past. Drop such a frame and re-anchor — the trail
    // just skips it, and the junction (`current`) stays put rather than jerking.
    let plausible = snap.true_vel.length() as f64 * dt as f64 * TRAIL_JUMP_SLACK + TRAIL_JUMP_FLOOR;
    let continuous = prev_pos.is_none_or(|p| p.distance(snap.true_pos) <= plausible);
    *prev_pos = Some(snap.true_pos);
    if continuous {
        let current = snap.true_pos;
        path.current = current;
        // Append when we've moved a step past the last record; thin by halving when full
        // (doubles the record's spacing everywhere — the line just spans the wider gaps,
        // so it stays continuous).
        let step = (radial_altitude(current.as_vec3()) as f64 * 0.01).max(TRAIL_MIN_STEP);
        if path.trail.last().is_none_or(|last| last.distance(current) >= step) {
            path.trail.push(current);
            if path.trail.len() > MAX_TRAIL {
                let mut keep = false;
                path.trail.retain(|_| {
                    keep = !keep;
                    keep
                });
            }
        }
    }

    // Future: re-propagate on a cadence into `future_target`, then ease `future_display`
    // toward it every frame (below) so a re-plan slides over smoothly. Stored as offsets
    // from the start (== the live position) so the eased line stays glued to the vehicle.
    *replan_in -= dt;
    if *replan_in <= 0.0 {
        *replan_in = REPLAN_SECS;
        let pos = snap.true_pos.as_vec3();
        let future_abs: Vec<DVec3> = if snap.throttle > 0.0 {
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
            preview.path.iter().map(|p| p.as_dvec3()).collect()
        } else {
            // Engines cut: the plan ahead is the ballistic coast (gravity + residual drag).
            path.plan = None;
            coast_path(pos, snap.true_vel, snap.vehicle.mass)
        };
        let start = future_abs.first().copied().unwrap_or(snap.true_pos);
        let offsets: Vec<DVec3> = future_abs.iter().map(|p| *p - start).collect();
        path.future_target = resample(&offsets, FUTURE_SAMPLES);
    }

    // Ease the drawn shape toward the target. Snap on the first fill / a length change
    // (a fresh launch, where there's nothing to ease from — easing from an empty/old
    // shape would draw a line collapsing onto the vehicle).
    if path.future_display.len() != path.future_target.len() {
        path.future_display = path.future_target.clone();
    } else {
        let alpha = (1.0 - (-FUTURE_EASE_RATE * dt).exp()) as f64;
        // Reborrow to a plain `&mut` so the two fields split-borrow (they don't through
        // `ResMut`'s `Deref`).
        let path = &mut *path;
        for (display, target) in path.future_display.iter_mut().zip(&path.future_target) {
            *display = display.lerp(*target, alpha);
        }
    }
}

/// Resample a polyline to exactly `n` points, evenly by index (the samples are already
/// uniform in flight time, so index ≈ look-ahead time). Lets two different-length plans
/// be eased point-for-point. `n >= 2`; a shorter/degenerate input is padded by repetition.
fn resample(path: &[DVec3], n: usize) -> Vec<DVec3> {
    match path.len() {
        0 => vec![DVec3::ZERO; n],
        1 => vec![path[0]; n],
        len => {
            let last = len - 1;
            (0..n)
                .map(|i| {
                    let s = i as f64 / (n - 1) as f64 * last as f64;
                    let lo = s.floor() as usize;
                    let hi = (lo + 1).min(last);
                    path[lo].lerp(path[hi], s - lo as f64)
                })
                .collect()
        }
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

/// Rebuild the line mesh: fold the true-frame path into room-local coordinates and
/// build a thin 3D tube (roughly constant on-screen width) following it.
fn draw_flight_path(
    mut commands: Commands,
    path: Res<FlightPath>,
    frame: Option<Res<ClientRoomFrame>>,
    // The MAIN camera specifically: the outline post-process spawns a second
    // `Camera3d` (its offscreen mask camera), so a bare `With<Camera3d>` single()
    // sees two and errors out every frame — which silently hid the whole line.
    // The main camera is the one egui bound its primary context to.
    camera: Query<&GlobalTransform, (With<Camera3d>, With<PrimaryEguiContext>)>,
    mut line: Query<(Entity, &mut Visibility), With<TrajectoryLine>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let Ok((entity, mut visibility)) = line.single_mut() else {
        return;
    };
    let Ok(camera) = camera.single() else {
        return;
    };
    if path.trail.is_empty() && path.future_display.is_empty() {
        *visibility = Visibility::Hidden; // idle: nothing to draw
        return;
    }
    // trail → current position → eased plan, one polyline in local coordinates. The visual
    // frame mirror keeps `true − offset` continuous through a rebase snap.
    let offset = frame.map(|f| f.offset).unwrap_or(DVec3::ZERO);
    let current_local = path.current - offset;
    let mut points: Vec<Vec3> =
        Vec::with_capacity(path.trail.len() + path.future_display.len() + 1);
    points.extend(path.trail.iter().map(|p| (p - offset).as_vec3()));
    points.push(current_local.as_vec3());
    // The future is offsets from the live position; re-anchor to it here so the eased
    // line stays glued to the vehicle. `future_display[0]` is ≈ the junction — skip it.
    points.extend(
        path.future_display
            .iter()
            .skip(1)
            .map(|o| (current_local + *o).as_vec3()),
    );

    let cam = camera.translation();
    let Some(mesh) = tube_mesh(&points, cam) else {
        *visibility = Visibility::Hidden;
        return;
    };
    *visibility = Visibility::Visible;
    // A fresh asset per rebuild (see [`TrajectoryLine`] for why not in-place mutation);
    // replacing the `Mesh3d` handle drops the previous frame's mesh.
    commands.entity(entity).insert(Mesh3d(meshes.add(mesh)));
}

/// Build a thin tube following `points`: one [`TUBE_SIDES`]-gon ring per point (radius
/// scaled by the point's camera distance), consecutive rings stitched into a pipe. The
/// ring frame is carried along the path by **parallel transport** — each ring's basis is
/// the previous one re-orthogonalised against the new tangent — so there is no twist or
/// kink even where the tangent swings through a world axis. Unlike a camera-facing
/// billboard, a tube never collapses edge-on. `None` when the path is too short to draw.
///
/// First the points are de-duplicated (a stalled assembly re-records the same spot; a
/// zero-length segment has no tangent), so the frame math always sees a real direction.
fn tube_mesh(points: &[Vec3], cam: Vec3) -> Option<Mesh> {
    let mut pts: Vec<Vec3> = Vec::with_capacity(points.len());
    for &p in points {
        if p.is_finite() && pts.last().is_none_or(|&last| last.distance(p) > 1e-3) {
            pts.push(p);
        }
    }
    if pts.len() < 2 {
        return None;
    }

    let n = pts.len();
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(n * TUBE_SIDES);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(n * TUBE_SIDES);
    // Seed the transported frame with any vector perpendicular to the first tangent.
    let first_tan = (pts[1] - pts[0]).normalize_or(Vec3::Y);
    let mut side = first_tan.any_orthonormal_vector();
    for i in 0..n {
        // Tangent: centred difference inside, one-sided at the ends.
        let tan = match i {
            0 => pts[1] - pts[0],
            _ if i == n - 1 => pts[n - 1] - pts[n - 2],
            _ => pts[i + 1] - pts[i - 1],
        }
        .normalize_or(first_tan);
        // Parallel transport: project the carried `side` onto the plane perpendicular to
        // the new tangent (removes twist), renormalise, rebuild the orthonormal frame.
        side = (side - tan * side.dot(tan)).normalize_or(tan.any_orthonormal_vector());
        let up = tan.cross(side);
        let radius = (pts[i].distance(cam) * LINE_RADIUS_PER_M).max(MIN_RADIUS);
        for s in 0..TUBE_SIDES {
            let a = s as f32 / TUBE_SIDES as f32 * std::f32::consts::TAU;
            let dir = side * a.cos() + up * a.sin();
            positions.push((pts[i] + dir * radius).to_array());
            normals.push(dir.to_array());
        }
    }

    let mut indices: Vec<u32> = Vec::with_capacity((n - 1) * TUBE_SIDES * 6);
    for i in 0..n - 1 {
        let (ra, rb) = ((i * TUBE_SIDES) as u32, ((i + 1) * TUBE_SIDES) as u32);
        for s in 0..TUBE_SIDES as u32 {
            let s1 = (s + 1) % TUBE_SIDES as u32;
            // Quad (ra+s, ra+s1, rb+s1, rb+s) → two outward-wound triangles.
            indices.extend_from_slice(&[ra + s, rb + s, ra + s1, ra + s1, rb + s, rb + s1]);
        }
    }
    Some(
        Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
            .with_inserted_indices(Indices::U32(indices)),
    )
}
