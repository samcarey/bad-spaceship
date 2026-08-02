//! The flight trajectory line: a yellow dotted path through the sky showing where the
//! launched assembly **has been** (the flown trail) and where the autopilot **plans to
//! take it** (the ascent plan re-propagated from the live state) — look up at liftoff
//! and the dots bend over ahead of you where the gravity turn will lean.
//!
//! Both halves live in the **true planet frame** (the trail in f64 — at Mm-scale
//! altitudes f32 steps by whole metres) and are folded back to room-local coordinates
//! through the room's visual floating-origin frame each frame, so a rebase slides the
//! line rigidly with the rest of the world. The future half is a drag-aware point-mass
//! forecast of the **pitch program the autopilot is holding** ([`propagate_program`]),
//! re-run from the current state **every frame** — so the line converges on what the
//! vehicle actually does rather than freezing the launch-day plan; after the escape cutoff
//! it shows the engines-off ballistic coast instead. Forecasting the *optimizer's* law
//! here instead (`propagate`) is what made the drawn path visibly wander during the climb:
//! that law steers prograde, so it re-extrapolated the live velocity direction over the
//! whole remaining burn every replan — see [`propagate_program`] for the measurements.
//!
//! **Why every frame and not on a timer.** The forecast used to be refreshed every 0.5 s,
//! which left it frozen in space while the vehicle flew on — and a real stack does not
//! track the point-mass model exactly (it is planned against *derated* thrust, and it
//! lags its attitude command). The craft therefore drifted off the drawn line for half a
//! second, and each re-propagation snapped the line back onto it: a 2 Hz sawtooth in the
//! line's position relative to the rocket, reported from a real ride. Rebuilding is only
//! **~25 µs** (0.15% of a 16.7 ms frame — the burn ends at the escape cutoff, ~700 integrator
//! steps, not the full budget), so there was never anything to save; the timer bought a
//! visible artifact for nothing. Note that easing the line toward each new forecast would
//! only *smooth* that snap — the line would still be a stale forecast, lagging rather than
//! jumping. Removing the staleness is the fix; smoothing it is makeup.
//!
//! The line is **transparent where you are**: fully clear inside [`FADE_HOLD_M`] of
//! path length either side of the vehicle, then ramping to full over the next
//! [`FADE_RAMP_M`], so the tube never paints over the rocket you are watching (or the
//! view from aboard it) while the plan further out stays legible. The ramp is
//! per-vertex colour on an alpha-blended material — the fade is measured in path length
//! along the line, not distance from the camera, so it stays put as the camera orbits.
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
    propagate_program, PitchProgram, GROUND_RADIUS, OPTIMIZER_DT, OPTIMIZER_STEPS,
};
use bad_spaceship_shared::map::{drag_force, gravity_at, radial_altitude, PLANET_CENTER};
use bad_spaceship_shared::net::TICK;
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

/// Line thickness: tube radius as a fraction of the camera distance (so it holds a
/// roughly constant on-screen width from the pad to a plan-end tens of km up), floored so
/// the near end never collapses to nothing.
const LINE_RADIUS_PER_M: f32 = 0.00075;
const MIN_RADIUS: f32 = 0.0125;
/// Cross-section sides of the tube. Few: it's a thin line, so it only needs to read as
/// round-ish from any angle, not be smooth.
const TUBE_SIDES: usize = 5;

/// Fully transparent inside this much path length either side of the vehicle — a hard
/// clear core, so nothing is drawn over the rocket you're watching (or over the view
/// from aboard it) no matter how the camera sits.
const FADE_HOLD_M: f32 = 30.0;
/// Path length over which the line then ramps in, starting at the edge of the clear
/// core — smoothstepped, so it emerges gradually rather than switching on at a ring.
/// Full strength at `FADE_HOLD_M + FADE_RAMP_M` ahead and behind.
const FADE_RAMP_M: f32 = 50.0;
/// Vertex spacing to resample the path to *inside* the fade window. Alpha is a
/// per-vertex quantity, so the profile can only be as faithful as the spacing it is
/// evaluated at, and the path's own spacing grows with the flight (see [`tube_mesh`]).
const FADE_STEP_M: f32 = 4.0;

/// The recorded + predicted flight path, in true planet-frame coordinates.
#[derive(Resource, Default)]
pub struct FlightPath {
    /// Where the assembly has been this launch (oldest first).
    trail: Vec<DVec3>,
    /// Where the plan takes it from here (re-propagated from the live state each frame).
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
            // Neon (electric highlighter) yellow — the scene has no bloom, so an unlit
            // fully-saturated bright yellow is what reads as "neon".
            base_color: Color::srgb(0.95, 1.0, 0.05),
            unlit: true,
            // The near-the-vehicle fade rides in per-vertex alpha, which only means
            // anything if the material blends (StandardMaterial multiplies base_color
            // by the mesh's vertex colour). Blended geometry doesn't write depth, so
            // the tube's own far side shows through — harmless, and it reads as glow.
            alpha_mode: AlphaMode::Blend,
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
    autopilot: Res<Autopilot>,
    mut path: ResMut<FlightPath>,
    // The pitch program being flown, rebuilt only when the launch's planning seed changes
    // (i.e. once per launch): sampling the table walks the whole ideal ascent, so
    // rebuilding it every frame would dominate the preview's cost for an identical answer.
    mut flown: Local<Option<PitchProgram>>,
) {
    let Some(snap) = autopilot.0.as_ref() else {
        // Launch over (or broke up): clear everything so the next launch starts fresh.
        if !path.trail.is_empty() || !path.future.is_empty() || path.plan.is_some() {
            *path = FlightPath::default();
        }
        *flown = None;
        return;
    };

    // Trail: append when we've moved a step past the last record; thin by halving when
    // full (doubles the record's spacing everywhere — the ribbon just spans the wider
    // gaps, so it stays continuous).
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

    let pos = snap.true_pos.as_vec3();
    if snap.throttle > 0.0 {
        // Burning: forecast the pitch program the autopilot is *holding*, from the live
        // state — same command schedule, same escape cutoff, so the line only moves as the
        // vehicle's own state moves (see `propagate_program` for what re-running the ideal
        // closed-loop law here cost instead).
        if flown.as_ref().is_none_or(|p| p.seed != snap.seed) {
            *flown = Some(PitchProgram::build(snap.seed));
        }
        let program = flown.as_ref().expect("just built");
        let preview = propagate_program(
            pos,
            snap.true_vel,
            snap.vehicle,
            program,
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

/// Rebuild the line mesh: fold the true-frame path into room-local coordinates and
/// build a thin 3D tube (roughly constant on-screen width) following it.
fn draw_flight_path(
    mut commands: Commands,
    path: Res<FlightPath>,
    autopilot: Res<Autopilot>,
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
    // trail → current position → plan, one polyline in local coordinates, folded by
    // the SAME frame the path was recorded with (`AutopilotSnapshot::frame_offset`) so
    // the line's anchor is exactly the rendered rocket. Using the visual
    // `ClientRoomFrame` here instead hung the line tens of metres off the rocket for a
    // packet or two after every rebase (reported above 2 km — the first rebase): the
    // snapshot folds with the tick-exact frame, and the two disagree exactly then.
    let offset = autopilot.0.as_ref().map_or_else(
        // No live snapshot: nothing anchors the path, so the stale visual frame is as
        // good as it gets (the path is cleared on the next update anyway).
        || frame.map(|f| f.offset).unwrap_or(DVec3::ZERO),
        |snap| snap.frame_offset,
    );
    let mut points: Vec<Vec3> = Vec::with_capacity(path.trail.len() + path.future.len() + 1);
    points.extend(path.trail.iter().map(|p| (p - offset).as_vec3()));
    // Where the trail hands over to the plan — the vehicle itself, and so the centre of
    // the transparent gap. Clamped for the snapshot-less frame (the path is stale and
    // about to be cleared; fading around its end is as meaningful as anything).
    let mut anchor = points.len();
    if let Some(snap) = autopilot.0.as_ref() {
        // One step forward from the snapshot. The thrust systems publish it from
        // `FixedUpdate` — *before* the physics step whose output the rocket is actually
        // drawn at — so the raw snapshot position is exactly one tick stale, a lag that
        // grows with speed (19 m at 1.2 km/s) and drags the line, and the clear core with
        // it, backwards off the vehicle. Integrating the step is exact to within one
        // `a·dt²` (~6 mm under full thrust).
        let step = snap.true_vel * TICK.as_secs_f32();
        points.push(((snap.true_pos + step.as_dvec3()) - offset).as_vec3());
    }
    // `future[0]` is the propagation start — the current position again; skip it.
    points.extend(path.future.iter().skip(1).map(|p| (p - offset).as_vec3()));
    anchor = anchor.min(points.len().saturating_sub(1));

    let cam = camera.translation();
    let Some(mesh) = tube_mesh(&points, anchor, cam) else {
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
///
/// `anchor` indexes the vehicle's own point in `points`: alpha is zero within
/// [`FADE_HOLD_M`] of arc length either side of it and smoothsteps to one over the next
/// [`FADE_RAMP_M`], carried as vertex colour (see the module doc for why the gap exists).
fn tube_mesh(points: &[Vec3], anchor: usize, cam: Vec3) -> Option<Mesh> {
    let mut pts: Vec<Vec3> = Vec::with_capacity(points.len());
    // Track the anchor across the de-duplication; if the anchor point *is* the duplicate
    // that got dropped, the kept twin (within 1 mm) stands in for it.
    let mut anchor_pt = 0;
    for (i, &p) in points.iter().enumerate() {
        if p.is_finite() && pts.last().is_none_or(|&last| last.distance(p) > 1e-3) {
            pts.push(p);
        }
        if i == anchor {
            anchor_pt = pts.len().saturating_sub(1);
        }
    }
    if pts.len() < 2 {
        return None;
    }

    let n = pts.len();
    // Cumulative arc length along the path. The fade is symmetric about the vehicle, so
    // only each point's absolute distance from the anchor's arc length matters.
    let mut arc: Vec<f32> = Vec::with_capacity(n);
    let mut run = 0.0;
    arc.push(0.0);
    for i in 1..n {
        run += pts[i].distance(pts[i - 1]);
        arc.push(run);
    }
    let anchor_arc = arc[anchor_pt];

    // Resample the fade window. Alpha lives on vertices and interpolates linearly
    // between them, so the fade profile is only drawn where the path HAS vertices — and
    // the path's spacing grows as the flight goes on: the plan samples every 0.5 s of
    // flight (64 m at 129 m/s, 600 m at 1.2 km/s) and the trail records every 1% of
    // altitude. One segment straddling the clear core therefore smears alpha linearly
    // straight across it: at 129 m/s the line is already 36% opaque 30 m ahead of the
    // rocket, where the profile calls for 0. Splitting only the stretch inside the
    // window keeps the cost bounded (~40 extra rings) — beyond it alpha is a flat 1 and
    // the original vertices are all it needs.
    let window = FADE_HOLD_M + FADE_RAMP_M;
    let mut fine: Vec<(Vec3, f32)> = Vec::with_capacity(n + 64);
    fine.push((pts[0], arc[0]));
    for i in 1..n {
        let (a0, a1) = (arc[i - 1], arc[i]);
        let seg = a1 - a0;
        let hi = (anchor_arc + window).min(a1);
        let mut s = (anchor_arc - window).max(a0);
        while s < hi {
            if s > a0 + 1e-3 && s < a1 - 1e-3 {
                fine.push((pts[i - 1].lerp(pts[i], (s - a0) / seg), s));
            }
            s += FADE_STEP_M;
        }
        fine.push((pts[i], a1));
    }
    let (pts, arc): (Vec<Vec3>, Vec<f32>) = fine.into_iter().unzip();
    let n = pts.len();

    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(n * TUBE_SIDES);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(n * TUBE_SIDES);
    let mut colors: Vec<[f32; 4]> = Vec::with_capacity(n * TUBE_SIDES);
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
        // Clear core, then a smoothstepped ramp so the line emerges rather than
        // switching on at a hard ring.
        let t = (((arc[i] - anchor_arc).abs() - FADE_HOLD_M) / FADE_RAMP_M).clamp(0.0, 1.0);
        let alpha = t * t * (3.0 - 2.0 * t);
        for s in 0..TUBE_SIDES {
            let a = s as f32 / TUBE_SIDES as f32 * std::f32::consts::TAU;
            let dir = side * a.cos() + up * a.sin();
            positions.push((pts[i] + dir * radius).to_array());
            normals.push(dir.to_array());
            colors.push([1.0, 1.0, 1.0, alpha]);
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
            .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
            .with_inserted_indices(Indices::U32(indices)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::VertexAttributeValues;
    use bevy::math::DVec3;
    use crate::launch::AutopilotSnapshot;

    /// Run `update_flight_path` over a launch whose velocity vector wobbles by a degree
    /// between replans, and return how far the drawn path's far end moved.
    ///
    /// A real ascent wobbles constantly (attitude lag, a rider shifting weight, contact
    /// noise), so this is the everyday case — not an edge one.
    fn end_movement_on_a_wobble() -> f32 {
        use bad_spaceship_shared::map::SURFACE_GRAVITY;

        let gravity = Vec3::new(0.0, -SURFACE_GRAVITY, 0.0);
        let (engines, mass) = (4usize, 15.5f32);
        let seed = PitchProgram::plan(
            Vec3::new(0.0, 1.0, 0.0), Vec3::ZERO, engines, gravity, mass, None,
        )
        .seed;
        // Mid-climb, well above TURN_SPEED — where the ideal law switches to prograde and
        // so becomes maximally sensitive to the velocity direction.
        let pos = DVec3::new(0.0, 2000.0, 0.0);
        let vel = Vec3::new(20.0, 180.0, 0.0);
        let snapshot = |true_vel: Vec3| {
            Some(AutopilotSnapshot {
                true_pos: pos,
                frame_offset: DVec3::ZERO,
                true_vel,
                vehicle: seed.vehicle,
                seed,
                command_angle: 0.0,
                throttle: 1.0,
                drag: 0.0,
                net_thrust: 0.0,
            })
        };

        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<FlightPath>()
            .init_resource::<Autopilot>()
            .add_systems(Update, update_flight_path);

        fn run(app: &mut App, v: Vec3, snap: impl Fn(Vec3) -> Option<AutopilotSnapshot>) -> DVec3 {
            app.world_mut().resource_mut::<Autopilot>().0 = snap(v);
            app.update();
            *app.world().resource::<FlightPath>().future.last().expect("a forecast")
        }
        let before = run(&mut app, vel, snapshot);
        let after = run(&mut app, Quat::from_rotation_z(1.0_f32.to_radians()) * vel, snapshot);
        before.distance(after) as f32
    }

    /// The trajectory line must not leap ahead of the rocket when the vehicle wobbles.
    ///
    /// This pins the *call site* to the flown pitch program (`propagate_program`). Drawing
    /// the optimizer's ideal law here instead re-extrapolates the live velocity direction
    /// over the whole remaining burn — one degree of wobble then moved the far end of the
    /// line by kilometres, and it landed somewhere new every half second for the whole
    /// climb. `guidance::the_drawn_forecast_holds_still_while_the_ideal_law_wanders` is the
    /// shared-side twin that measures the two laws directly.
    #[test]
    fn the_line_ahead_does_not_leap_on_a_wobble() {
        let moved = end_movement_on_a_wobble();
        assert!(
            moved < 100.0,
            "a 1° velocity wobble moved the drawn path's end by {moved:.0} m",
        );
    }

    /// The forecast must start at the rocket on **every** frame, not on a timer.
    ///
    /// Regression for a 2 Hz sawtooth reported from a real ride: the future half was
    /// re-propagated every 0.5 s, so between refreshes it hung frozen in space while the
    /// vehicle flew on. Because a real stack does not track the point-mass model exactly
    /// (planned against derated thrust; attitude lags the command), the rocket drifted off
    /// the drawn line and each re-propagation snapped it back — the line visibly sliding
    /// against the craft twice a second. This flies a few frames at 60 Hz and demands the
    /// forecast be re-anchored at the live position each one; under the timer, every frame
    /// but the first kept the stale start and this fails by the distance flown since.
    #[test]
    fn the_forecast_starts_at_the_rocket_every_frame() {
        use bad_spaceship_shared::map::SURFACE_GRAVITY;

        let gravity = Vec3::new(0.0, -SURFACE_GRAVITY, 0.0);
        let (engines, mass) = (4usize, 15.5f32);
        let seed =
            PitchProgram::plan(Vec3::new(0.0, 1.0, 0.0), Vec3::ZERO, engines, gravity, mass, None)
                .seed;
        let vel = Vec3::new(20.0, 280.0, 0.0);

        let mut app = App::new();
        app.init_resource::<FlightPath>()
            .init_resource::<Autopilot>()
            .add_systems(Update, update_flight_path);

        let dt = 1.0 / 60.0;
        let mut pos = DVec3::new(0.0, 3000.0, 0.0);
        for frame in 0..10 {
            pos += vel.as_dvec3() * dt as f64;
            app.world_mut().resource_mut::<Autopilot>().0 = Some(AutopilotSnapshot {
                true_pos: pos,
                frame_offset: DVec3::ZERO,
                true_vel: vel,
                vehicle: seed.vehicle,
                seed,
                command_angle: 0.0,
                throttle: 1.0,
                drag: 0.0,
                net_thrust: 0.0,
            });
            app.update();
            // `future[0]` is the propagation start — by contract the vehicle's own
            // position, which the draw pass skips in favour of the live one.
            let start = app.world().resource::<FlightPath>().future[0];
            let stale = start.distance(pos);
            assert!(
                stale < 0.01,
                "frame {frame}: the forecast starts {stale:.2} m from the rocket",
            );
        }
    }

    /// Ring centres and their alpha, for a path built along +Y. The ring offsets are
    /// perpendicular to the tangent — which is +Y here — so every vertex of a ring
    /// shares the ring's `y`, and the height doubles as the arc length.
    fn rings(mesh: &Mesh) -> Vec<(f32, f32)> {
        let Some(VertexAttributeValues::Float32x3(pos)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("no positions")
        };
        let Some(VertexAttributeValues::Float32x4(col)) = mesh.attribute(Mesh::ATTRIBUTE_COLOR)
        else {
            panic!("no colours")
        };
        pos.iter()
            .zip(col)
            .step_by(TUBE_SIDES)
            .map(|(p, c)| (p[1], c[3]))
            .collect()
    }

    /// The fade profile must be *drawn*, not merely sampled wherever the path happens to
    /// have vertices. Alpha is a vertex attribute and interpolates linearly between
    /// rings, while the path's own spacing grows with the flight (the plan samples every
    /// 0.5 s — 600 m at 1.2 km/s — and the trail records every 1% of altitude). Before
    /// the window was resampled, one segment straddling the clear core smeared alpha
    /// straight across it: 36% opaque 30 m ahead of the rocket, where the profile calls
    /// for zero. This asserts the window is sampled finely enough to represent it.
    #[test]
    fn the_clear_core_survives_a_coarsely_sampled_path() {
        // A straight climb sampled every 300 m, the vehicle on the middle vertex.
        let points: Vec<Vec3> = (0..7).map(|i| Vec3::new(0.0, i as f32 * 300.0, 0.0)).collect();
        let mesh = tube_mesh(&points, 3, Vec3::new(50.0, 900.0, 0.0)).expect("a drawable tube");
        let rings = rings(&mesh);

        // Check the alpha the GPU actually rasterises — linearly interpolated between
        // the bracketing rings — not merely the values sitting on the rings. Testing
        // ring spacing instead would be both self-referential (FADE_STEP_M is the
        // constant under test) and vacuous when the window holds a single sample.
        let shaded = |d: f32| {
            let y = 900.0 + d;
            let hi = rings.iter().position(|&(ry, _)| ry >= y).expect("bracketed");
            if hi == 0 {
                return rings[0].1;
            }
            let ((y0, a0), (y1, a1)) = (rings[hi - 1], rings[hi]);
            a0 + (a1 - a0) * ((y - y0) / (y1 - y0))
        };
        for step in -80..=80 {
            let d = step as f32;
            let want = {
                let t = ((d.abs() - FADE_HOLD_M) / FADE_RAMP_M).clamp(0.0, 1.0);
                t * t * (3.0 - 2.0 * t)
            };
            let got = shaded(d);
            assert!(
                (got - want).abs() < 0.05,
                "at {d} m the line renders at alpha {got:.2}, profile calls for {want:.2}"
            );
        }
    }
}
