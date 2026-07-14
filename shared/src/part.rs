use crate::map::PLATFORM_WIDTH_M;
use crate::utils::{self, QuatExt, Vec3Ext};
use crate::{
    AttachEvent, Attachable, BoundingRadius, CameraOrbitCenter, DisplayableJoint, ExistingJoints,
    FocusedInteractable, HoldPoint, Holding, Modifying, Player, PlayerClick, PlayerHoldPoint,
    PotentialJoints, PredeleteJoint, PredeleteJoints, ToggleHoldingSystemLabel, UpdateJointsLabel,
};
use avian3d::collision::collider::contact_query::contact_manifolds;
use avian3d::prelude::{
    AngularVelocity, Collider, ColliderDensity, Collisions, Forces, Friction,
    Gravity, JointCollisionDisabled, LinearVelocity, Position, ReadRigidBodyForces, Restitution,
    Rotation,
    ComputedMass, RigidBody, SphericalJoint, SweptCcd, WriteRigidBodyForces,
};
use bevy::prelude::*;
use rand::prelude::ThreadRng;
use rand::Rng;
use std::f32;

pub struct PartPlugin;

/// Marker resource inserted by the client in multiplayer mode. While present,
/// the local part *creation* systems are skipped: the client renders the
/// server's replicated parts instead of simulating its own. The authoritative
/// server (which never inserts this) keeps simulating the shared part world.
#[derive(Resource)]
pub struct SuppressLocalParts;

impl Plugin for PartPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Startup,
            (spawn_initial_parts, spawn_initial_rocket_engines)
                .run_if(not(resource_exists::<SuppressLocalParts>)),
        )
            // Recycling runs in `FixedUpdate`, not `Update`: it also catches parts
            // whose state diverged (see `part_state_diverged`), which must be
            // despawned before the *next* physics step — Avian's broadphase asserts
            // on a NaN AABB. `FixedUpdate` precedes the step every tick, while an
            // `Update` system can be skipped between two back-to-back fixed steps.
            .add_systems(
                FixedUpdate,
                (replace_fallen_parts, replace_fallen_rocket_engines)
                    .run_if(not(resource_exists::<SuppressLocalParts>)),
            )
            // Weld-rigidity census: runs in EVERY mode (the multiplayer client's
            // predicted joints need the same collision filtering as the server's,
            // or the predicted contact set diverges from the authoritative one).
            .add_systems(FixedUpdate, maintain_weld_rigidity)
            .add_systems(
                Update,
                (
                    // Single-player focus. MUST be off in multiplayer: with zero
                    // `Interactable` entities there (replicated parts are `Holdable`
                    // only) it unconditionally clears `FocusedInteractable` every
                    // frame, racing the multiplayer `update_focus` writer with
                    // compile-dependent system order — on losing builds the focus
                    // highlight and the grab gate were dead in multiplayer.
                    update_focused.run_if(not(resource_exists::<SuppressLocalParts>)),
                    // Avian's `Forces` helper auto-clears after the physics step, so
                    // the old per-frame `zero_part_external_forces` system is gone.
                    // Both force systems write through `Forces` on the held part, so
                    // order them to avoid an ambiguous double-write.
                    position_held_part,
                    orient_held_part.after(position_held_part),
                    spawn_part.run_if(not(resource_exists::<SuppressLocalParts>)),
                    update_attachable,
                    (update_active_joints, update_predelete_joints)
                        .in_set(UpdateJointsLabel)
                        .before(ToggleHoldingSystemLabel),
                    attach
                        .after(ToggleHoldingSystemLabel)
                        .after(UpdateJointsLabel),
                    // In multiplayer the joints are replicated from the server, so
                    // deletion is server-authoritative (`server_delete`); a local
                    // despawn here would corrupt the replicated joint replica.
                    delete_joints
                        .after(ToggleHoldingSystemLabel)
                        .after(UpdateJointsLabel)
                        .run_if(not(resource_exists::<SuppressLocalParts>)),
                ),
            )
            .add_message::<NewPart>()
            .init_resource::<PotentialJoints>()
            .init_resource::<ExistingJoints>()
            .init_resource::<PredeleteJoints>();
    }
}

// Number of parts in a world. `pub` so the server can spawn one set per room
// (multiplayer per-room world isolation) rather than a single shared set.
pub const NUM_PARTS: i32 = 10;
pub const MAX_PART_SIZE: f32 = 10.0;
const MIN_PART_SIZE: f32 = 0.1;
const MIN_PART_VOLUME: f32 = 1.0;
const MAX_PART_VOLUME: f32 = 2.0;
/// Uniform density of every dynamic part (cuboids *and* rockets); mass = density ×
/// volume. `pub` so the rocket-thrust visualisation can weigh thrust against an
/// average part.
pub const PART_DENSITY: f32 = 2.0;
/// Mass of a nominal "average" part — the mean of the accepted volume band times the
/// shared density. Used to size rocket thrust ("lift N average parts").
pub const NOMINAL_PART_MASS: f32 = PART_DENSITY * (MIN_PART_VOLUME + MAX_PART_VOLUME) / 2.0;
// Held-part spring stiffnesses — also used by the multiplayer hold helpers in
// `net.rs` (the server runs the same critically-damped springs), so they're
// `pub` to keep a single definition rather than duplicated magic numbers.
pub const POSITIONING_STIFFNESS: f32 = 30.0;
pub const ORIENTING_STIFFNESS: f32 = 20.0;
const MIN_JOINT_SPACING: f32 = MIN_PART_SIZE / 2.0;
pub const DELETE_RADIUS: f32 = 1.0;

/// Whether a joint anchor is **interior** — at (≈) its body's local origin rather than
/// on a touching face — so its green marker would float inside the opaque body (e.g.
/// old hand-built stacks whose deck↔rocket welds are anchored at the rocket's center,
/// which read as "a green dot in the middle of the cylinder"). Interior welds are
/// load-bearing (they give a connection its lever arm; a stack falls apart without
/// them — ride-verified), so only their *markers* are suppressed, never the joints:
/// the persistent multiplayer sphere (`bind_replicated_joints`) and the held-part
/// green list (`update_active_joints`). The red predelete marker is deliberately NOT
/// filtered — it warns about an imminent deletion, and in single-player the deletion
/// acts on exactly that list. Surface anchors sit ≥ the part's half-extent (≥ 0.4 m
/// here) from the origin, well clear of this threshold.
pub fn is_interior_anchor(anchor: Vec3) -> bool {
    const INTERIOR_JOINT_EPS: f32 = 0.1;
    anchor.length_squared() < INTERIOR_JOINT_EPS * INTERIOR_JOINT_EPS
}

// Rocket-engine part geometry (a tall cylinder body with a flared nozzle at the
// base). `pub` so the client renderer builds the matching mesh from the same
// numbers the collider is built from here. The entity origin is the *body*
// centre; the flare hangs below it (the narrow end of the flare meets the body).
pub const NUM_ROCKET_ENGINES: i32 = 3;
pub const ROCKET_BODY_RADIUS: f32 = 0.4;
pub const ROCKET_BODY_HEIGHT: f32 = 1.8;
/// The flare's wide (bottom) radius; its narrow (top) radius is the body radius.
pub const ROCKET_FLARE_BOTTOM_RADIUS: f32 = 0.8;
pub const ROCKET_FLARE_HEIGHT: f32 = 0.7;
/// Local-Y offset from the body centre (the entity origin) to the flare centre.
/// Shared by the flare's collider (`spawn_rocket_engine`) and its child mesh (the
/// client renderer) so the physics and visual flare stay aligned.
pub const ROCKET_FLARE_Y_OFFSET: f32 = -(ROCKET_BODY_HEIGHT / 2.0 + ROCKET_FLARE_HEIGHT / 2.0);

/// Nominal thrust of one rocket engine, expressed as how many *average* parts it can
/// lift against gravity. Multiply by `NOMINAL_PART_MASS` and gravity's magnitude to
/// get the force in newtons. Feeds `launch::full_rocket_thrust` — the single source
/// for the real launch thrust AND the thrust-vector visualisation, so the arrow and
/// the force can't drift.
///
/// History: 3.0 at first, +10% by feel (3.3) — which made a bare engine TWR ≈ 5.5, so
/// cartoon-strong that a vertical burn escaped almost as cheaply as a gravity turn and
/// the fuel-optimal autopilot had nothing to optimize. Lowered to 2.0 to put typical
/// cargo-carrying builds at real first-stage thrust-to-weight (Saturn V lifted off at
/// TWR ~1.2): a loaded hauler now sits at TWR ~1.2–1.5 where a proper gravity turn
/// saves ~15–20% of its fuel, while an engine-dense low-payload build still reaches
/// TWR ≳2.5 and can brute-force straight up. Engine count and payload now *matter* to
/// how the autopilot flies (see `shared::guidance`).
pub const ROCKET_THRUST_PART_WEIGHTS: f32 = 2.0;

/// Local-frame point where a rocket's thrust is applied: inside the rocket at the
/// base of the body, where the flare begins (the flare's narrow top meets the body's
/// bottom face). The entity origin is the *body* centre, so this is `ROCKET_BODY_HEIGHT/2`
/// below it.
pub const ROCKET_THRUST_ORIGIN_LOCAL: Vec3 = Vec3::new(0.0, -(ROCKET_BODY_HEIGHT / 2.0), 0.0);

/// Local-frame thrust direction: up the cylinder axis, *away* from the flared end —
/// the reaction/lift direction the exhaust pushes the rocket.
pub const ROCKET_THRUST_DIR_LOCAL: Vec3 = Vec3::Y;

/// Approximate volume of a rocket engine (cylinder body + cone flare), used as a
/// mass proxy where parts are weighted by volume (density is uniform across all
/// parts) — e.g. the server's largest-assembly center-of-mass. The cuboid path
/// uses `8·hx·hy·hz`; this is the rocket's analogue so an assembly's COM stays
/// mass-accurate when rockets are jointed in.
pub const ROCKET_VOLUME: f32 = std::f32::consts::PI
    * ROCKET_BODY_RADIUS
    * ROCKET_BODY_RADIUS
    * ROCKET_BODY_HEIGHT
    + std::f32::consts::PI * ROCKET_FLARE_BOTTOM_RADIUS * ROCKET_FLARE_BOTTOM_RADIUS
        * ROCKET_FLARE_HEIGHT
        / 3.0;

/// Below this Y a part/rocket has fallen off the platform and is recycled.
/// `pub` so the multiplayer server's per-room recycler uses the same threshold.
pub const PART_FALL_Y: f32 = -10.0;

/// Whether a part's simulation state has *diverged* — non-finite or absurd
/// position/velocity from an exploding constraint solve (observed with a jointed
/// rocket assembly at extreme altitude, where f32 resolution starves the solver).
/// A NaN position panics Avian's next broadphase (`assertion failed:
/// b.min.cmple(b.max)`) and takes the whole app down, so recyclers check this
/// every tick *before* the physics step. The bounds are far beyond anything
/// legitimate play produces (the fastest verified healthy state is a rocket
/// ascent at ~3 km/s; nothing recoverable spins at 1000 rad/s or sits 1000 km
/// from a 50 m map).
pub fn part_state_diverged(position: Vec3, linear: Vec3, angular: Vec3) -> bool {
    !position.is_finite()
        || !linear.is_finite()
        || !angular.is_finite()
        || position.length_squared() > 1.0e12 // |pos| > 1000 km
        || linear.length_squared() > 1.0e10 // |v| > 100 km/s
        || angular.length_squared() > 1.0e6 // |w| > 1000 rad/s
}

#[derive(Default, Component)]
struct Interactable;

#[derive(Default, Component)]
pub struct Holdable;

/// Marks a part as a rocket engine rather than a random cuboid. The physics and
/// grab/join logic are collider-agnostic, so a rocket engine behaves like any
/// other part; this marker only steers *rendering* (the client draws a
/// cylinder+flare instead of a cuboid, and skips the cuboid renderer for it).
#[derive(Default, Component)]
pub struct RocketEngine;

/// The rocket's current thrust-vector deflection: a local-frame nozzle tilt vector
/// whose direction is which way the thrust tips off the body axis (local x/z) and
/// whose length is the tilt angle in radians (≤ `launch::GIMBAL_MAX_RAD`). The launch
/// autopilot slews it toward a commanded deflection at the gimbal's rate limit each
/// physics tick (`launch::gimbal_step`) — it's a real actuator with travel and slew
/// limits, not an instant one. Local integrator state on every side (server, SP,
/// predicted MP), not replicated: each side follows the same command law from the
/// same measured state, so they converge like the throttle trim does.
#[derive(Default, Component, Debug, Clone, Copy)]
pub struct Gimbal(pub Vec2);

/// The part's random-appearance seed, minted at spawn. The client derives the
/// whole metal look (tint, brushing, flakes, scratches) deterministically from
/// it; in multiplayer it rides `NetPart` so every client renders the same part
/// identically.
#[derive(Component, Clone, Copy)]
pub struct PartSeed(pub u32);

#[derive(Default, Component)]
struct GetsReplaced;

struct CriticallyDampedHarmonicOscillator {
    stiffness: f32,
    damping: f32,
}

impl CriticallyDampedHarmonicOscillator {
    pub fn new(stiffness: f32) -> Self {
        Self {
            stiffness,
            damping: 2.0 * stiffness.sqrt(),
        }
    }

    pub fn calculate_acceleration(&self, displacement: &Vec3, velocity: &Vec3) -> Vec3 {
        *displacement * self.stiffness - *velocity * self.damping
    }
}

#[derive(Component)]
pub struct TargetPosition {
    pub hold_point_entity: Entity,
    oscillator: CriticallyDampedHarmonicOscillator,
}

impl TargetPosition {
    pub fn new(hold_point_entity: Entity) -> Self {
        Self {
            hold_point_entity,
            oscillator: CriticallyDampedHarmonicOscillator::new(POSITIONING_STIFFNESS),
        }
    }
}

#[derive(Component)]
pub struct TargetOrientation {
    pub quat: Quat,
    oscillator: CriticallyDampedHarmonicOscillator,
}

impl TargetOrientation {
    pub fn new(quat: Quat) -> Self {
        Self {
            quat,
            oscillator: CriticallyDampedHarmonicOscillator::new(ORIENTING_STIFFNESS),
        }
    }
}

const SPAWN_ZONE_HALF_WIDTH: f32 = PLATFORM_WIDTH_M / 2.0 * 0.7;

#[derive(Bundle, Default)]
struct PartBundle {
    interactable: Interactable,
    holdable: Holdable,
    gets_replaced: GetsReplaced,
    // Avian splits rapier's `Velocity` into two components; mass/inertia are
    // computed automatically from the collider + density (no `ReadMassProperties`
    // to carry), and forces are applied via the `Forces` query helper (no
    // `ExternalForce` component).
    linear_velocity: LinearVelocity,
    angular_velocity: AngularVelocity,
}

#[derive(Message)]
struct NewPart;

fn get_random_shape(rng: &mut ThreadRng) -> Collider {
    loop {
        let (x, y, z) = (
            rng.gen_range(MIN_PART_SIZE..=MAX_PART_SIZE),
            rng.gen_range(MIN_PART_SIZE..=MAX_PART_SIZE),
            rng.gen_range(MIN_PART_SIZE..=MAX_PART_SIZE),
        );
        let volume = x * y * z;
        if volume < MAX_PART_VOLUME && volume > MIN_PART_VOLUME {
            // Avian's `Collider::cuboid` takes FULL extents (rapier's cuboid took
            // half-extents, hence the old `/ 2.0`); the resulting box is identical.
            return Collider::cuboid(x, y, z);
        }
    }
}

fn spawn_part(mut commands: Commands, mut new_part_events: MessageReader<NewPart>) {
    for _ in new_part_events.read() {
        spawn_random_part(&mut commands);
    }
}

/// Spawn one random dynamic part (the standard collider + physics props) at a
/// random spawn-zone position, returning its entity and the cuboid's
/// half-extents plus its appearance seed. Shared by the single-player spawner
/// and the multiplayer server's per-room spawner; the caller adds any extra
/// tagging (replication, room membership, collision layers). The half-extents
/// and seed let the server fill `NetPart` without re-reading components after
/// the spawn flushes. Owns its RNG so the server doesn't need to depend on
/// `rand` (a `ThreadRng` is a cheap thread-local handle).
pub fn spawn_random_part(commands: &mut Commands) -> (Entity, Vec3, u32) {
    let mut rng = rand::thread_rng();
    let collider = get_random_shape(&mut rng);
    // Every random shape is a cuboid (see `get_random_shape`); recover its
    // half-extents for `NetPart`. Falls back to a unit box if that ever changes.
    let half_extents = collider
        .shape()
        .as_cuboid()
        .map(|c| Vec3::new(c.half_extents[0], c.half_extents[1], c.half_extents[2]))
        .unwrap_or(Vec3::ONE);
    let spawn = Vec3::new(
        rng.gen_range(-SPAWN_ZONE_HALF_WIDTH..=SPAWN_ZONE_HALF_WIDTH),
        rng.gen_range(5.0..=15.0),
        rng.gen_range(-SPAWN_ZONE_HALF_WIDTH..=SPAWN_ZONE_HALF_WIDTH),
    );
    let seed = rng.gen();
    let mut e = commands.spawn_empty();
    insert_part_physics(&mut e, half_extents);
    e.insert((
        PartSeed(seed),
        // Bevy 0.15: bare `Transform` (it now requires `GlobalTransform`).
        // Set Avian `Position` too, not just `Transform`: in multiplayer the server
        // disables Avian's `PhysicsTransformPlugin` (lightyear_avian owns the sync),
        // so a spawn `Transform` alone is NOT copied into `Position` — the body would
        // simulate from the origin and every part would cluster in the middle of the
        // stage. Seeding `Position` matches `build_server_avatar`. Harmless in
        // single-player (both are set to the same pose).
        Transform::from_translation(spawn),
        Position(spawn),
        PartBundle::default(),
    ));
    (e.id(), half_extents, seed)
}

/// Spawn one cuboid part with **explicit** half-extents and appearance seed — the
/// deterministic counterpart of `spawn_random_part`, used to rebuild parts from a
/// saved world. Deliberately sets no pose: the caller inserts the saved
/// `Transform`/`Position`/`Rotation`/velocities (both `Transform` *and* the Avian
/// components must be seeded in multiplayer — see `spawn_random_part`).
pub fn spawn_saved_cuboid(commands: &mut Commands, half_extents: Vec3, seed: u32) -> Entity {
    let mut e = commands.spawn_empty();
    insert_part_physics(&mut e, half_extents);
    e.insert((PartSeed(seed), PartBundle::default()));
    e.id()
}

/// Insert the shared dynamic-part physics (collider + mass/friction/restitution +
/// CCD) onto an entity from its cuboid half-extents. Used by `spawn_random_part`
/// (single-player + the server's authoritative parts) AND the multiplayer client's
/// predicted-part setup, so both ends simulate an *identical* body — essential for
/// client-side prediction to stay close to the server (state replication only
/// corrects divergence; matching physics keeps that divergence tiny).
pub fn insert_part_physics(entity: &mut EntityCommands, half_extents: Vec3) {
    entity.insert((
        // The cuboid's bounding-sphere radius (centre at origin → `half_extents.norm()`,
        // identical to parry's `compute_local_bounding_sphere().radius`). Attached here,
        // in the *shared* part-physics helper, so every part carries it from one source —
        // single-player/server (`spawn_random_part`) AND the multiplayer client's
        // predicted parts (`draw_replicated_parts`) — rather than only the spawner. The
        // pickup camera/hold-point "feel" reads this, so it now works in both modes.
        BoundingRadius(half_extents.length()),
        // Avian's `Collider::cuboid` takes FULL extents (= 2 × half_extents).
        Collider::cuboid(half_extents.x * 2.0, half_extents.y * 2.0, half_extents.z * 2.0),
    ));
    insert_part_dynamics(entity);
}

/// The physics props every dynamic part shares regardless of shape: the
/// rigid-body kind, the density/friction/restitution tuning, and CCD. Both the
/// cuboid `insert_part_physics` and the rocket engine's `spawn_rocket_engine`
/// apply these, so the one tuning set lives in a single place; each caller adds
/// its own shape-specific `Collider` + `BoundingRadius`.
fn insert_part_dynamics(entity: &mut EntityCommands) {
    entity.insert((
        RigidBody::Dynamic,
        // rapier's `ColliderMassProperties::Density` / `Friction::coefficient` /
        // `Restitution::coefficient` → Avian's `ColliderDensity` / `Friction::new`
        // / `Restitution::new`.
        ColliderDensity(PART_DENSITY),
        Friction::new(1.0),
        Restitution::new(0.1),
        // Parts spawn high and hit the thin trimesh ground fast; without CCD a fast
        // impact penetrates deeply in one solver step and soft-contact recovery
        // leaves the body embedded. CCD catches it so parts rest flush.
        SweptCcd::default(),
    ));
}

fn spawn_initial_parts(mut new_part_events: MessageWriter<NewPart>) {
    for _ in 0..NUM_PARTS {
        new_part_events.write(NewPart);
    }
}

/// Spawn one dynamic rocket-engine part at `position`. It's `Holdable` +
/// `Interactable` like a cuboid part, so it grabs and joins identically — the
/// only difference is the collider shape (a compound of the cylinder body plus a
/// cone for the flared nozzle) and, on the client, the mesh drawn for it. The
/// entity origin is the body centre; the flare's collider is offset below so the
/// engine rests upright on its nozzle. Returns the spawned entity.
///
/// **Single-player only for now:** the spawner is gated on `SuppressLocalParts`,
/// so rockets are never created in multiplayer. Networking them needs a shape
/// discriminant on `NetPart` (which today carries cuboid `half_extents` only) —
/// deferred until rockets go multiplayer or a second rendered part type lands.
pub fn spawn_rocket_engine(commands: &mut Commands, position: Vec3) -> Entity {
    let mut entity = commands.spawn((
        Interactable,
        Holdable,
        LinearVelocity::default(),
        AngularVelocity::default(),
        // Seed both `Transform` and Avian `Position` (see `spawn_random_part`).
        Transform::from_translation(position),
        Position(position),
    ));
    insert_rocket_physics(&mut entity);
    entity.id()
}

/// Insert the rocket-engine shape's physics onto an existing entity: the `RocketEngine`
/// marker, the compound (cylinder body + cone flare) collider, its bounding radius, and
/// the shared dynamic-part props. Factored out of `spawn_rocket_engine` so the
/// multiplayer client's replicated-part path (`draw_replicated_parts`) rebuilds an
/// *identical* body from `NetPart` — the rocket analogue of `insert_part_physics` for
/// cuboids, essential for client-side prediction to stay close to the server.
pub fn insert_rocket_physics(entity: &mut EntityCommands) {
    // Flare cone: Avian's `Collider::cone` is centred on its own origin, base
    // (wide) at -Y and apex at +Y — so offset it below the body (`ROCKET_FLARE_Y_OFFSET`)
    // with the apex (narrow end) meeting the body's bottom face.
    let collider = Collider::compound(vec![
        (Vec3::ZERO, Quat::IDENTITY, Collider::cylinder(ROCKET_BODY_RADIUS, ROCKET_BODY_HEIGHT)),
        (
            Vec3::new(0.0, ROCKET_FLARE_Y_OFFSET, 0.0),
            Quat::IDENTITY,
            Collider::cone(ROCKET_FLARE_BOTTOM_RADIUS, ROCKET_FLARE_HEIGHT),
        ),
    ]);
    // Bounding radius (for the camera/hold-point "feel"): the farthest point from
    // the body centre is the flare's bottom rim.
    let flare_bottom_y = ROCKET_BODY_HEIGHT / 2.0 + ROCKET_FLARE_HEIGHT;
    let bounding_radius = (ROCKET_FLARE_BOTTOM_RADIUS.powi(2) + flare_bottom_y.powi(2)).sqrt();
    entity.insert((RocketEngine, Gimbal::default(), collider, BoundingRadius(bounding_radius)));
    // Density/friction/restitution/CCD/rigid-body — shared with the cuboid parts.
    insert_part_dynamics(entity);
}

/// Spawn one rocket engine at a random spawn-zone position — the multiplayer server's
/// per-room rocket spawner (mirrors `spawn_random_part` for cuboids). Returns the entity
/// so the caller can tag it for room-scoped replication.
pub fn spawn_random_rocket(commands: &mut Commands) -> Entity {
    let mut rng = rand::thread_rng();
    spawn_rocket_engine(commands, random_rocket_spawn(&mut rng))
}

fn random_rocket_spawn(rng: &mut ThreadRng) -> Vec3 {
    Vec3::new(
        rng.gen_range(-SPAWN_ZONE_HALF_WIDTH..=SPAWN_ZONE_HALF_WIDTH),
        rng.gen_range(5.0..=12.0),
        rng.gen_range(-SPAWN_ZONE_HALF_WIDTH..=SPAWN_ZONE_HALF_WIDTH),
    )
}

fn spawn_initial_rocket_engines(mut commands: Commands) {
    let mut rng = rand::thread_rng();
    for _ in 0..NUM_ROCKET_ENGINES {
        spawn_rocket_engine(&mut commands, random_rocket_spawn(&mut rng));
    }
}

/// Whether this part should be recycled: fallen off the platform, or its state
/// diverged (see `part_state_diverged` — recycling before the next physics step
/// is what keeps a solver explosion from panicking Avian's broadphase).
fn needs_recycle(position: &Position, linear: &LinearVelocity, angular: &AngularVelocity) -> bool {
    position.0.y < PART_FALL_Y || part_state_diverged(position.0, linear.0, angular.0)
}

/// A rocket engine that falls off the platform (or diverges) is despawned and a
/// fresh one dropped back in — the rocket-engine counterpart to
/// `replace_fallen_parts` (rockets carry no `GetsReplaced`, since that path
/// respawns a *cuboid*).
fn replace_fallen_rocket_engines(
    mut commands: Commands,
    rockets: Query<(&Position, &LinearVelocity, &AngularVelocity, Entity), With<RocketEngine>>,
) {
    let mut rng = rand::thread_rng();
    for (position, linear, angular, entity) in rockets.iter() {
        if needs_recycle(position, linear, angular) {
            commands.entity(entity).despawn();
            spawn_rocket_engine(&mut commands, random_rocket_spawn(&mut rng));
        }
    }
}

/// Maintain [`JointCollisionDisabled`] per welded pair: collision between two jointed
/// bodies is turned off exactly when the joint graph already makes their relative pose
/// **rotationally rigid**, and kept on otherwise.
///
/// Rigidity is a property of the **rigid cluster**, not just the pair. Two levels:
/// - A pair with 3+ non-collinear anchors is a rigid weld (as before).
/// - A body whose joints INTO one rigid cluster total 3+ non-collinear anchors — even
///   spread across several single-joint pairs — is pinned to that cluster just as hard:
///   each anchor point is fixed in the cluster's frame. The census unions rigid pairs
///   into clusters, then absorbs such bodies (to a fixpoint), and disables collision on
///   every jointed pair that ends up inside one cluster. The per-pair-only census missed
///   this: a rocket bolted by one joint each to the deck and to two base rockets (all
///   mutually rigid) formed a *rigid loop of hinges* — every pair kept its contact, and
///   the joint and contact solvers fought around the loop. Recorder-verified on the
///   "6 rocks" save: the loop rattled the base rockets to |ω| ≈ 9.6 rad/s at blastoff
///   (5 cm/tick relative jitter) and kicked the whole stack over before the autopilot
///   could settle.
///
/// Why both halves matter (both recorder-verified):
/// - A rigid weld that also *touches* (player-built joints form exactly where parts
///   touch) puts a contact and a joint set on the same pair; Avian solves contacts
///   and XPBD joints in different passes, and on a rigid pair they "correct" each
///   other's mm-scale disagreement every substep — a deck bridging two rockets in
///   real contact pumped up and exploded ~30 s after every blastoff. Contact adds
///   nothing physical to a rigid weld (the joints fully own the relative pose), so
///   it is dropped. Joint compliance (1e-5…1e-4) and doubled solver substeps were
///   both tried first and neither stops the pump.
/// - A 1-2-point weld is a pivot/hinge, and its contact is **structure**: a part
///   welded by two points along an edge is braced flat by the face it touches (it
///   may swing *away*, never *through*). Disabling contact there turns builds
///   floppy — and un-footed ground clamps let a whole launch pad pendulum over the
///   moment a rider stepped aboard. A *dangling* hinge (1-2 anchors into its cluster)
///   still keeps its contact — only hinges the cluster proves rigid lose it.
///
/// The census reruns whenever joints change (attach, delete-zone, blastoff clamp
/// cut), so deleting joints off a rigid weld automatically re-arms its contact.
fn maintain_weld_rigidity(
    mut commands: Commands,
    joints: Query<(Entity, &SphericalJoint, Has<LockJoint>)>,
    changed: Query<(), Changed<SphericalJoint>>,
    mut removed: RemovedComponents<SphericalJoint>,
) {
    if changed.is_empty() && removed.read().next().is_none() {
        return;
    }
    // Group each pair's joints, keeping each joint's anchor in BOTH bodies' own frames
    // (joints between the same two bodies may disagree on body1/body2 order; normalize
    // to the pair's smaller-entity-first key).
    type PairJoints = Vec<(Entity, Vec3, Vec3)>; // (joint, anchor in key.0, anchor in key.1)
    let mut pairs: std::collections::HashMap<(Entity, Entity), PairJoints> =
        std::collections::HashMap::new();
    let mut lock_pairs: std::collections::HashSet<(Entity, Entity)> =
        std::collections::HashSet::new();
    for (entity, joint, is_lock) in &joints {
        let (key, a_anchor, b_anchor) = if joint.body1 <= joint.body2 {
            ((joint.body1, joint.body2), joint.local_anchor1(), joint.local_anchor2())
        } else {
            ((joint.body2, joint.body1), joint.local_anchor2(), joint.local_anchor1())
        };
        // A rider's lock weld owns its pair's relative pose outright (centre-anchored,
        // rotation-free by design) — its contact adds nothing but a capsule-vs-deck
        // fight while the tilting body sweeps through the surface. Always drop it,
        // regardless of the anchor-count rigidity the census would compute.
        if is_lock {
            lock_pairs.insert(key);
        }
        pairs.entry(key).or_default().push((
            entity,
            a_anchor.unwrap_or_default(),
            b_anchor.unwrap_or_default(),
        ));
    }

    // Union-find over the jointed bodies (the same `DisjointSet` the assembly grouping
    // uses). Clusters form purely by absorption below: a per-pair rigid weld is just
    // the two-body case (a's 3+ anchors into b's singleton cluster), so it needs no
    // separate seeding pass.
    let mut index: std::collections::HashMap<Entity, usize> = std::collections::HashMap::new();
    for &(a, b) in pairs.keys() {
        for body in [a, b] {
            let next = index.len();
            index.entry(body).or_insert(next);
        }
    }
    let mut clusters = crate::assembly::DisjointSet::new(index.len());

    // Absorb, to a fixpoint: a body whose joints into ONE cluster pin 3+ non-collinear
    // of its own points is rigid to that cluster — merge it in. (Each merge can make
    // further bodies absorbable; the joint graphs here are tiny, so the loop is cheap.)
    loop {
        // body index → (other cluster root → this body's anchors into that cluster).
        let mut into_cluster: std::collections::HashMap<
            usize,
            std::collections::HashMap<usize, Vec<Vec3>>,
        > = std::collections::HashMap::new();
        for (&(a, b), members) in &pairs {
            let (ia, ib) = (index[&a], index[&b]);
            let (ra, rb) = (clusters.find(ia), clusters.find(ib));
            if ra == rb {
                continue;
            }
            for &(_, a_anchor, b_anchor) in members {
                into_cluster.entry(ia).or_default().entry(rb).or_default().push(a_anchor);
                into_cluster.entry(ib).or_default().entry(ra).or_default().push(b_anchor);
            }
        }
        let mut merged = false;
        for (&body, roots) in &into_cluster {
            for (&root, anchors) in roots {
                if anchors_are_rigid(anchors.iter().copied())
                    && clusters.find(body) != clusters.find(root)
                {
                    clusters.union(body, root);
                    merged = true;
                }
            }
        }
        if !merged {
            break;
        }
    }

    for (&(a, b), members) in &pairs {
        let rigid = lock_pairs.contains(&(a, b))
            || clusters.find(index[&a]) == clusters.find(index[&b]);
        for &(entity, _, _) in members {
            // `try_insert`/`try_remove` (not `insert`/`remove`): a joint queried here can
            // be despawned before these deferred commands apply — the floating-origin
            // rebase cuts ground joints, and part churn (a diverged part recycled, an
            // avatar joining mid-flight jostling the stack) can drop joints — and a plain
            // `insert`/`remove` on the despawned entity panics the whole app
            // (`panic=abort`). The despawn-tolerant variants no-op on a dead entity.
            if rigid {
                commands.entity(entity).try_insert(JointCollisionDisabled);
            } else {
                commands.entity(entity).try_remove::<JointCollisionDisabled>();
            }
        }
    }
}

/// Whether a set of anchor points pins *all* relative rotation: 3+ points spanning a
/// non-degenerate triangle. 1 point = ball pivot, 2 (or collinear) = hinge — those
/// pairs keep their contact as bracing.
fn anchors_are_rigid(anchors: impl Iterator<Item = Vec3>) -> bool {
    let points: Vec<Vec3> = anchors.collect();
    if points.len() < 3 {
        return false;
    }
    for i in 0..points.len() {
        for j in (i + 1)..points.len() {
            for k in (j + 1)..points.len() {
                let area2 =
                    (points[j] - points[i]).cross(points[k] - points[i]).length_squared();
                if area2 > 1e-8 {
                    return true;
                }
            }
        }
    }
    false
}

fn replace_fallen_parts(
    mut commands: Commands,
    parts: Query<(&Position, &LinearVelocity, &AngularVelocity, Entity), With<GetsReplaced>>,
    mut new_part_events: MessageWriter<NewPart>,
) {
    for (position, linear, angular, entity) in parts.iter() {
        if needs_recycle(position, linear, angular) {
            commands.entity(entity).despawn();
            new_part_events.write(NewPart);
        }
    }
}

// Focus range / look-angle for selecting a part — also used by the multiplayer
// `focused_part` helper in `net.rs`, so they're `pub` (single definition).
pub const MAX_INTERACT_DISTANCE: f32 = 7.5;
const MAX_INTERACT_DISTANCE_SQUARED: f32 = MAX_INTERACT_DISTANCE * MAX_INTERACT_DISTANCE;
const MAX_INTERACT_ANGLE_DEGREES: f32 = 20.0;
pub const MAX_INTERACT_ANGLE: f32 = MAX_INTERACT_ANGLE_DEGREES * utils::DEG_TO_RADIANS;

fn update_focused(
    mut players: Query<(&mut FocusedInteractable, &Holding, &Children), With<Player>>,
    mut interactables: Query<(&mut Transform, Entity), With<Interactable>>,
    camera_orbit_centers: Query<&GlobalTransform, With<CameraOrbitCenter>>,
) {
    // Determine which iteractable entity each player is focused on (i.e. looking at, within range)
    for (mut focused_interactable, holding, player_children) in players.iter_mut() {
        if !holding.0 {
            let mut newly_focused_interactable_option = None;
            // Focus is independent of the modifier: a grabbable block stays
            // highlighted even while the delete zone is shown (the modifier no
            // longer toggles between "focus to grab" and "delete mode"; on touch the
            // delete zone is always on when empty-handed, and grabbing is selected by
            // the click itself — see `mobile::apply_pointer`). Pickup still requires
            // the modifier off (`player::toggle_holding`), so the two never collide.
            {
                for player_child in player_children.iter() {
                    if let Ok(camera_orbit_center) = camera_orbit_centers.get(player_child) {
                        // Search for the most appropriate interactable that should be focused by the player
                        let mut smallest_angle = MAX_INTERACT_ANGLE;

                        for (interactable_transform, interactable) in interactables.iter_mut() {
                            let vector_between = interactable_transform.translation
                                - camera_orbit_center.translation();
                            if vector_between.length_squared() < MAX_INTERACT_DISTANCE_SQUARED {
                                let angle_from_look =
                                    camera_orbit_center.back().angle_between(vector_between);

                                if angle_from_look < smallest_angle {
                                    smallest_angle = angle_from_look;
                                    newly_focused_interactable_option = Some(interactable);
                                }
                            }
                        }
                    }
                }
            }

            focused_interactable.0 = newly_focused_interactable_option;
        }
    }
}

fn update_attachable(
    mut commands: Commands,
    helds: Query<Entity, With<TargetPosition>>,
    holdables: Query<(), With<Holdable>>,
    attachables: Query<Entity, (With<Holdable>, With<Attachable>)>,
    not_attachables: Query<Entity, (With<Holdable>, Without<Attachable>)>,
    collisions: Collisions,
) {
    if let Some(held) = helds.iter().next() {
        let contacted = collisions
            .collisions_with(held)
            .filter(|pair| pair.is_touching())
            // Avian's `ContactPair::collider1/2` are plain `Entity` (rapier's were
            // `Option`); take the other collider in each touching pair.
            .map(|pair| {
                if pair.collider1 == held {
                    pair.collider2
                } else {
                    pair.collider1
                }
            })
            .filter(|&contacted| holdables.get(contacted).is_ok())
            .collect::<Vec<_>>();
        for not_attachable in not_attachables.iter() {
            if contacted.contains(&not_attachable) {
                commands.entity(not_attachable).insert(Attachable);
            }
        }
        for attachable in attachables.iter() {
            if !contacted.contains(&attachable) {
                commands.entity(attachable).remove::<Attachable>();
            }
        }
    } else {
        for attachable in attachables.iter() {
            commands.entity(attachable).remove::<Attachable>();
        }
    }
}

fn position_held_part(
    hold_points: Query<&GlobalTransform, With<HoldPoint>>,
    // `Forces` (no `&`/`&mut`) is Avian's per-frame force helper; it accumulates
    // during the physics step and auto-clears afterwards (rapier's `ExternalForce`
    // had to be zeroed each frame). It takes `LinearVelocity`/`AngularVelocity`
    // mutably internally, so it can't share a query with `&LinearVelocity` — read
    // the velocity off the helper instead.
    mut parts: Query<(&Transform, &TargetPosition, Forces)>,
    // Avian's global gravity is a `Res<Gravity>` (rapier read it off the per-world
    // `RapierConfiguration` component).
    gravity: Res<Gravity>,
) {
    for (part_transform, target_position, mut forces) in parts.iter_mut() {
        if let Ok(hold_point_position) = hold_points.get(target_position.hold_point_entity) {
            let vector_between = hold_point_position.translation() - part_transform.translation;
            let velocity = forces.linear_velocity();
            let positioning_acceleration = target_position
                .oscillator
                .calculate_acceleration(&vector_between, &velocity);
            // Apply as an acceleration so Avian handles the mass conversion;
            // subtracting gravity cancels the part's weight so it floats to the hold
            // point (rapier set force = mass·(accel − gravity) explicitly).
            forces.apply_linear_acceleration(positioning_acceleration - gravity.0);
        }
    }
}

fn orient_held_part(mut parts: Query<(&Transform, &TargetOrientation, Forces)>) {
    for (part_transform, target_orientation, mut forces) in parts.iter_mut() {
        let rotation_between =
            (target_orientation.quat * part_transform.rotation.conjugate()).to_rotation_vector();
        let angular_velocity = forces.angular_velocity();
        let angular_acceleration = target_orientation
            .oscillator
            .calculate_acceleration(&rotation_between, &angular_velocity);
        // Apply as angular acceleration; Avian converts it to torque via the body's
        // inertia tensor. (rapier multiplied by the principal-inertia vector
        // explicitly — the held-part orientation response may feel slightly
        // different but is now physically consistent.)
        forces.apply_angular_acceleration(angular_acceleration);
    }
}

/// Marks a joint as a **player-lock weld** — a `SphericalJoint` pinning a player's
/// avatar (`body1`) to a part (`body2`) it was touching when the player pressed
/// "Lock". Exists in every world that simulates the constraint: the server spawns it
/// (with its replicated `NetLockJoint` mirror), each multiplayer client re-tags the
/// joint it rebuilds between its *predicted* avatar/part, and single-player spawns it
/// directly. The marker is what exempts these welds from every part-joint sweep that
/// must not touch them — the blastoff ground-joint cut (an avatar endpoint isn't a
/// part, so the cut would otherwise sever riders at liftoff) and the delete-zone
/// gesture — and what the movement systems consult to freeze a locked rider's
/// walk/jump (a velocity write would fight the weld every tick).
#[derive(Component, Default)]
pub struct LockJoint;

/// Whether `body` is currently pinned by a player-lock weld (each weld's `body1` is
/// the avatar). Movement systems skip locked bodies entirely — the weld owns their
/// velocity — and they consult the live joint set (not a cached marker) so the skip
/// appears/disappears on exactly the tick the joint does, identically on the server
/// and the predicting client (rollback replays included). A linear `any` over the
/// handful of welds: allocation-free, which matters in the per-tick movement systems
/// that re-run for every replayed tick during a rollback.
pub fn is_locked(lock_joints: &Query<&SphericalJoint, With<LockJoint>>, body: Entity) -> bool {
    lock_joints.iter().any(|joint| joint.body1 == body)
}

/// Despawn every lock weld pinning `avatar` — the "unlock" / teleport-teardown
/// primitive, shared by the server (unlock requests, teleports) and the
/// single-player client (the Unlock button, respawn cleanup).
pub fn despawn_player_lock_welds(
    commands: &mut Commands,
    lock_joints: &Query<(Entity, &SphericalJoint), With<LockJoint>>,
    avatar: Entity,
) {
    for (weld, joint) in lock_joints {
        if joint.body1 == avatar {
            commands.entity(weld).despawn();
        }
    }
}

/// Gap-weld contacts between an avatar body and each candidate part: the one
/// definition of *what a lock welds to* — the same gap-tolerant, freeze-in-place
/// contact manifold parts attach with ([`part_gap_contacts`]) — shared by the
/// server's authoritative lock and the single-player client so the two can't drift.
/// `parts` yields each candidate (the caller applies its own room/held-part
/// filtering); `weld` is called once per contact with `(part, avatar-local anchor,
/// part-local anchor)` and spawns whatever joint bundle its world needs.
pub fn avatar_lock_contacts<'a>(
    avatar: (&Collider, Vec3, Quat),
    parts: impl Iterator<Item = (Entity, &'a Collider, Vec3, Quat)>,
    mut weld: impl FnMut(Entity, Vec3, Vec3),
) {
    // Anchor on the avatar side at the CENTRE OF THE BOTTOM HEMISPHERE (the rider's
    // feet), not the capsule centre: the pill pivots about its feet planted on the
    // deck, like a standing person, rather than hinging at the waist. Computed once —
    // it's a fixed point in the avatar's local frame.
    let foot_local = capsule_bottom_center(avatar.0);
    // Its world position now (the avatar rotates to the rider's felt up each tick — see
    // `FeltUp` — but the deck rotates with the assembly, so the two rotate together and
    // the feet stay planted). The rider's collider is disabled while locked (see
    // `toggle_locked_rider_collision`), so there is no capsule-vs-deck contact fighting
    // the weld regardless of where the anchor sits.
    let foot_world = avatar.1 + avatar.2 * foot_local;
    let mut contacts = Vec::new();
    for (part, collider, position, rotation) in parts {
        contacts.clear();
        part_gap_contacts(avatar.0, avatar.1, avatar.2, collider, position, rotation, &mut contacts);
        if contacts.is_empty() {
            continue;
        }
        // ONE weld per touched part: the feet anchor pinned to the deck point under them.
        let part_anchor = rotation.inverse() * (foot_world - position);
        weld(part, foot_local, part_anchor);
    }
}

/// Centre of a capsule collider's bottom hemisphere in the collider's local frame — the
/// lower endpoint of the capsule's inner segment (Avian's capsule runs along local +Y,
/// centred at the origin). Falls back to the origin (the capsule centre) for any other
/// shape, so a non-capsule rider still gets a sane centre anchor.
fn capsule_bottom_center(collider: &Collider) -> Vec3 {
    collider.shape().as_capsule().map_or(Vec3::ZERO, |c| {
        let (a, b) = (c.segment.a, c.segment.b);
        let p = if a[1] <= b[1] { a } else { b };
        Vec3::new(p[0], p[1], p[2])
    })
}

/// Drop lock welds whose endpoints no longer exist: the avatar despawned
/// (disconnect, single-player fall respawn) or the welded part got recycled/reset
/// away. Registered by the server unconditionally and by the client only in
/// single-player (`not(resource_exists::<SuppressLocalParts>)`) — a multiplayer
/// client must never locally despawn the replicated welds the server owns. A
/// vanished weld is also what flips the derived "locked" state back to false
/// everywhere, so a gone player never gates a room's launch.
pub fn cleanup_lock_joints(
    mut commands: Commands,
    lock_joints: Query<(Entity, &SphericalJoint), With<LockJoint>>,
    bodies: Query<(), With<Position>>,
) {
    for (entity, joint) in &lock_joints {
        if bodies.get(joint.body1).is_err() || bodies.get(joint.body2).is_err() {
            commands.entity(entity).despawn();
        }
    }
}

/// Fraction of the **relative** velocity (linear + angular) between two jointed bodies
/// bled off per physics tick — structural damping for welded assemblies.
///
/// Why: a contact and a joint set solving the same pair (or a rigid loop of hinges — see
/// [`maintain_weld_rigidity`]) can pump energy every substep; under the pitch program's
/// sustained lateral load a marginal pair's pump can run away until the whole assembly
/// detonates (recorder: six parts diverging at once, |ω| 400–1900 rad/s, mid-turn at
/// ~1.2 km — a player build lost this way). Force-based fixes were tried in the census
/// era and failed (joint compliance and doubled substeps both "neither stops the pump").
/// This damping is different in kind: each tick the pair's relative velocity is scaled
/// down directly, momentum-weighted, so it strictly REMOVES relative kinetic energy —
/// it cannot inject any, no matter how violent the pump — and a runaway must now outgrow
/// a 30%-per-tick drain to survive. Bodies a joint intends to be rigid have no
/// legitimate relative motion, so the aggressive rate costs nothing real; dangling
/// hinged parts just swing viscously instead of jangling.
const WELD_DAMPING_PER_TICK: f32 = 0.3;

/// Relative speed below which the damper does nothing (m/s linear, rad/s angular).
/// Normal flight keeps welded pairs within solver-noise of zero relative motion — a
/// damper acting there is a no-op physically but NOT numerically: on a predicted
/// multiplayer client it smears half-applied rollback corrections across the welded
/// cluster, injecting client-only velocity deltas that feed a rollback storm (observed:
/// ~18 rollbacks/s and a mid-flight breakup with a browser client attached, while
/// bot-only flights of the same binary flew clean). The dead-zone keeps the damper
/// silent until a pair shows REAL divergence — an actual pump — which is also why the
/// damper only registers on authoritative sims (server + single-player), never on the
/// predicted twin.
const WELD_DAMPING_DEADZONE: f32 = 0.5;

/// Apply [`WELD_DAMPING_PER_TICK`] across every jointed **dynamic** pair, once per pair
/// per tick (a pair welded by several joints is damped once). Skips pairs touching a
/// non-dynamic body: a static ground clamp partner ignores velocity writes for motion,
/// but scribbling on its `LinearVelocity` would leak into `GroundVelocity` support
/// readings. Runs identically on the server, single-player, and the predicted client
/// (the same shared-system discipline as the hold spring and gravity correction).
pub fn damp_weld_motion(
    joints: Query<&SphericalJoint>,
    mut bodies: Query<(&RigidBody, &mut LinearVelocity, &mut AngularVelocity, &ComputedMass)>,
) {
    let mut seen: std::collections::HashSet<(Entity, Entity)> = std::collections::HashSet::new();
    for joint in &joints {
        let key = if joint.body1 <= joint.body2 {
            (joint.body1, joint.body2)
        } else {
            (joint.body2, joint.body1)
        };
        if key.0 == key.1 || !seen.insert(key) {
            continue;
        }
        let Ok([(rb1, mut v1, mut w1, m1), (rb2, mut v2, mut w2, m2)]) =
            bodies.get_many_mut([key.0, key.1])
        else {
            continue;
        };
        if *rb1 != RigidBody::Dynamic || *rb2 != RigidBody::Dynamic {
            continue;
        }
        let (m1, m2) = (m1.value(), m2.value());
        let total = m1 + m2;
        if total <= 0.0 {
            continue;
        }
        // Momentum-conserving relative-velocity bleed: the light body yields more,
        // dead-zoned (see `WELD_DAMPING_DEADZONE`) so it acts only on real pumps. Linear
        // and angular are damped identically — one closure so they can't drift. (Spin
        // uses mass weighting as a cheap stand-in for inertia weighting; exact
        // conservation matters less than never *adding* energy, which holds for any
        // convex weighting.)
        let mut bleed = |a: &mut Vec3, b: &mut Vec3| {
            let rel = *b - *a;
            if rel.length_squared() > WELD_DAMPING_DEADZONE * WELD_DAMPING_DEADZONE {
                let d = rel * WELD_DAMPING_PER_TICK;
                *a += d * (m2 / total);
                *b -= d * (m1 / total);
            }
        };
        bleed(&mut v1.0, &mut v2.0);
        bleed(&mut w1.0, &mut w2.0);
    }
}

/// Maximum face separation (metres) at which two parts can still weld together.
/// Lets you join a part that's merely *close* to another without a pixel-perfect
/// touch — the common cause of a rocket stack forming too few joints to stay
/// upright. The parts are welded **where they sit** (see [`part_gap_contacts`]) —
/// they stay at their fixed separation, bridged by a rigid strut — so the weld
/// never yanks them together. Tunable by feel.
pub const JOINT_GAP: f32 = 0.1;

/// Minimum spacing (metres) between two welds of the SAME pair. The contact manifold
/// against a faceted surface (above all the concave trimesh ground bowl) yields one
/// point per overlapping triangle — dozens crammed together — which is over-constrained
/// and floods joint replication. So the welds are thinned: keep the closest contact,
/// then greedily add the point farthest from those already kept, until no candidate is
/// at least this far from every kept weld. Tuned (0.55) so a platform on a rocket's top
/// (cylinder ⌀0.8) still gets 4 welds — a rigid mount, not a hinge — while the faceted
/// ground bowl collapses to ~5-9 spread welds instead of ~20. Tunable.
///
/// Distinct from [`MIN_JOINT_SPACING`] (much smaller): that dedups a *new* weld against
/// the pair's *existing* joints; this thins welds *within* one fresh manifold.
pub const MIN_JOINT_DIST: f32 = 0.55;

/// Gap-tolerant weld points between two part colliders, returned as `(anchor_on_a,
/// anchor_on_b)` pairs in each body's **local (origin-relative)** frame — exactly
/// what [`SphericalJoint`] anchors want. Appends to `out`.
///
/// Uses a pure geometry contact-manifold query with a prediction distance of
/// [`JOINT_GAP`], so it (a) does **not** perturb the physics sim, and (b) yields the
/// full multi-point manifold even across a small gap — a flush near-approach still
/// produces the corner welds that make a stack rotationally rigid, instead of one
/// wobbly pivot. The points are then **thinned** to a minimum spacing (see
/// [`MIN_JOINT_DIST`]) so a faceted contact (the trimesh ground) can't explode into
/// dozens of welds.
///
/// Each weld freezes the two parts at their **current relative pose**: both anchors
/// are the material point on each body that currently sits at the contact midpoint,
/// so they coincide in world space *right now*. The weld therefore has **zero rest
/// error** — it holds the parts at their fixed separation (a rigid strut across the
/// gap) rather than snapping them together, and stores no hidden load, so it's
/// launch-stable at *any* approach angle. Parts are centred cuboids, so the local
/// anchor is `rot⁻¹ · (midpoint − position)` (independent of centre of mass).
pub fn part_gap_contacts(
    a_collider: &Collider,
    a_pos: Vec3,
    a_rot: Quat,
    b_collider: &Collider,
    b_pos: Vec3,
    b_rot: Quat,
    out: &mut Vec<(Vec3, Vec3)>,
) {
    // Enforce one "finite pose in" invariant at this shared boundary: a rollback can
    // transiently give a predicted part a non-finite position OR rotation before the
    // divergence recycler runs, and parry asserts on non-finite input — which would
    // crash the whole app.
    if !a_pos.is_finite() || !b_pos.is_finite() || !a_rot.is_finite() || !b_rot.is_finite() {
        return;
    }
    // `contact_manifolds` internally rejects far-apart shapes cheaply, so no manual
    // broad-phase pre-filter is needed for the handful of parts in a room.
    let mut manifolds = Vec::new();
    contact_manifolds(a_collider, a_pos, a_rot, b_collider, b_pos, b_rot, JOINT_GAP, &mut manifolds);

    // Gather every in-gap contact point: its world position (for thinning by distance),
    // separation (smallest = closest contact, the preferred seed), and body-local anchors.
    let (a_inv, b_inv) = (a_rot.inverse(), b_rot.inverse());
    let mut cands: Vec<(Vec3, f32, Vec3, Vec3)> = Vec::new();
    for manifold in &manifolds {
        for contact in &manifold.points {
            // `penetration` is positive when overlapping, so a separation of `g` is
            // `penetration = -g`; keep points within the weld gap.
            if contact.penetration < -JOINT_GAP || !contact.point.is_finite() {
                continue;
            }
            cands.push((
                contact.point,
                -contact.penetration,
                a_inv * (contact.point - a_pos),
                b_inv * (contact.point - b_pos),
            ));
        }
    }
    if cands.is_empty() {
        return;
    }

    // Thin to a minimum spacing. Seed with the closest contact (smallest separation),
    // then repeatedly add the candidate whose nearest already-kept weld is farthest —
    // stopping once no candidate is at least `MIN_JOINT_DIST` from every kept weld. This
    // is farthest-point (Poisson-disk) selection: it spreads the welds for maximum
    // rigidity and caps their count regardless of how faceted the contact is.
    let seed = cands
        .iter()
        .enumerate()
        .min_by(|(_, x), (_, y)| x.1.total_cmp(&y.1))
        .map(|(i, _)| i)
        .unwrap();
    let mut kept = vec![seed];
    loop {
        let mut best: Option<(usize, f32)> = None;
        for (i, cand) in cands.iter().enumerate() {
            if kept.contains(&i) {
                continue;
            }
            let nearest = kept
                .iter()
                .map(|&k| cands[k].0.distance(cand.0))
                .fold(f32::INFINITY, f32::min);
            if nearest >= MIN_JOINT_DIST && best.map_or(true, |(_, b)| nearest > b) {
                best = Some((i, nearest));
            }
        }
        match best {
            Some((i, _)) => kept.push(i),
            None => break,
        }
    }
    for &i in &kept {
        out.push((cands[i].2, cands[i].3));
    }
}

fn update_active_joints(
    mut potential_joints: ResMut<PotentialJoints>,
    mut existing_joints: ResMut<ExistingJoints>,
    players: Query<(&Holding, &FocusedInteractable)>,
    // Avian joints are standalone entities carrying `body1`/`body2` (rapier's
    // joint was a child of one body, with the other reached via `joint.parent`).
    joints: Query<&SphericalJoint>,
    // Every part's collider + authoritative pose, for the gap weld query
    // (`part_gap_contacts`). Includes the held part itself.
    parts: Query<(Entity, &Collider, &Position, &Rotation), With<Holdable>>,
    // The ground bowl is a weld candidate too — `RigidBody::Static` at the world origin
    // (identity), so its pose is a constant. `part_gap_contacts` thins the faceted-bowl
    // manifold to a spread rigid set, so no synthesized anchor triangle is needed.
    ground_q: Query<(Entity, &Collider), With<crate::Grass>>,
) {
    potential_joints.0.clear();
    existing_joints.0.clear();

    if let Some((holding, interactable)) = players.iter().next() {
        if holding.0 {
            if let Some(held_entity) = interactable.0 {
                // Existing joints on the held part → GREEN (`display_existing_joints`).
                // Read straight from the joint graph, NOT from live contacts: a gap
                // weld holds the parts apart (not touching), so a contact-based scan
                // would miss it and the joint would never turn green once it's real.
                // `entities.0` is the held part, which the display draws the sphere on.
                for joint in joints.iter() {
                    let (Some(a1), Some(a2)) = (joint.local_anchor1(), joint.local_anchor2())
                    else {
                        continue;
                    };
                    // Interior welds keep working but their marker would float inside
                    // the body — skip drawing it (see `is_interior_anchor`).
                    if is_interior_anchor(a1) || is_interior_anchor(a2) {
                        continue;
                    }
                    if joint.body2 == held_entity {
                        existing_joints.0.push(DisplayableJoint {
                            entities: (held_entity, joint.body1),
                            points: (a2, a1),
                        });
                    } else if joint.body1 == held_entity {
                        existing_joints.0.push(DisplayableJoint {
                            entities: (held_entity, joint.body2),
                            points: (a1, a2),
                        });
                    }
                }

                // Weld the held part to any nearby part OR the ground within `JOINT_GAP`,
                // using the thinned contact manifold (flush faces → a spread rigid set of
                // welds; the faceted bowl is thinned to a handful, so no special ground
                // path is needed). Each weld freezes the pair at its current relative pose
                // (zero rest error — see `part_gap_contacts`), so nothing is yanked.
                if let Ok((_, held_collider, held_pos, held_rot)) = parts.get(held_entity) {
                    let parts_iter = parts.iter().map(|(e, c, p, r)| (e, c, p.0, r.0));
                    let ground_iter =
                        ground_q.iter().map(|(e, c)| (e, c, Vec3::ZERO, Quat::IDENTITY));
                    let mut contacts = Vec::new();
                    for (other, other_collider, other_pos, other_rot) in parts_iter.chain(ground_iter) {
                        if other == held_entity {
                            continue;
                        }
                        contacts.clear();
                        part_gap_contacts(
                            held_collider,
                            held_pos.0,
                            held_rot.0,
                            other_collider,
                            other_pos,
                            other_rot,
                            &mut contacts,
                        );
                        for (held_local, other_local) in contacts.iter().copied() {
                            // Skip a weld coinciding with one this pair already has
                            // (`entities.0` is always the held part in `existing_joints`).
                            let duplicate = existing_joints.0.iter().any(|p| {
                                p.entities == (held_entity, other)
                                    && (p.points.0 - held_local).norm() <= MIN_JOINT_SPACING
                            });
                            if !duplicate {
                                potential_joints.0.push(DisplayableJoint {
                                    entities: (held_entity, other),
                                    points: (held_local, other_local),
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}

fn update_predelete_joints(
    holdables: Query<&GlobalTransform, With<Holdable>>,
    mut predelete_joints: ResMut<PredeleteJoints>,
    players: Query<(&Holding, &Modifying, &PlayerHoldPoint)>,
    // Player-lock welds are dissolved by "Unlock", never the delete gesture.
    joints: Query<(Entity, &SphericalJoint), Without<LockJoint>>,
    hold_points: Query<&GlobalTransform, With<HoldPoint>>,
) {
    predelete_joints.0.clear();

    if let Some((holding, modifying, hold_point)) = players.iter().next() {
        if !holding.0 && modifying.0 {
            if let Ok(hold_point_position) = hold_points.get(hold_point.0) {
                for (joint_entity, joint) in joints.iter() {
                    // World position of the joint's anchor on `body2` (rapier
                    // used the joint's parent body + `local_frame2`). Fall back to
                    // the `body1` anchor when `body2` isn't a holdable part — a
                    // ground joint's ground endpoint — the constraint pins both
                    // anchors to the same point, so either side works.
                    let anchor_world = |body, anchor: Option<Vec3>| {
                        holdables.get(body).ok().zip(anchor).map(|(transform, anchor)| {
                            let transform = transform.compute_transform();
                            transform.translation + transform.rotation.mul_vec3(anchor)
                        })
                    };
                    if let Some(center) = anchor_world(joint.body2, joint.local_anchor2())
                        .or_else(|| anchor_world(joint.body1, joint.local_anchor1()))
                    {
                        if (center - hold_point_position.translation()).length() < DELETE_RADIUS {
                            predelete_joints.0.push(PredeleteJoint {
                                entity: joint_entity,
                                translation: center,
                            });
                        }
                    }
                }
            }
        }
    }
}

fn attach(
    mut commands: Commands,
    mut attach_events: MessageReader<AttachEvent>,
    attach_points: Res<PotentialJoints>,
    joints: Query<&SphericalJoint>,
    mut new_part_events: MessageWriter<NewPart>,
) {
    if attach_events.read().next().is_some() {
        // Parts that already had at least one joint before this attach. A part that
        // gains its *first* joint is being consumed into a structure, so spawn a
        // fresh random part to replace it in the loose-parts pool (keeps building
        // from depleting the world). `commands.spawn` is deferred, so this query
        // reflects the pre-attach state.
        let had_joint: Vec<Entity> = joints.iter().flat_map(|j| [j.body1, j.body2]).collect();
        let mut replaced: Vec<Entity> = Vec::new();
        for DisplayableJoint { points, entities } in attach_points.0.iter() {
            // One SphericalJoint per (thinned) weld point — part-to-part AND part-to-ground
            // alike; the ground clamp is just the thinned manifold's points, no synthesized
            // triangle. Anchor mapping: body1/anchor1 = entities.1/points.1 and
            // body2/anchor2 = entities.0/points.0 — the convention `update_active_joints`
            // and the gizmo rendering read back.
            commands.spawn(
                SphericalJoint::new(entities.1, entities.0)
                    .with_local_anchor1(points.1)
                    .with_local_anchor2(points.0),
            );
            for endpoint in [entities.0, entities.1] {
                if !had_joint.contains(&endpoint) && !replaced.contains(&endpoint) {
                    replaced.push(endpoint);
                    new_part_events.write(NewPart);
                }
            }
        }
    }
}

fn delete_joints(
    mut commands: Commands,
    predelete_joints: Res<PredeleteJoints>,
    mut clicks: MessageReader<PlayerClick>,
) {
    if clicks.read().next().is_some() {
        for PredeleteJoint { entity, .. } in predelete_joints.0.iter() {
            // Bevy 0.16 made `despawn()` recursive by default (the old
            // `despawn_recursive()` is gone).
            commands.entity(*entity).despawn();
        }
    }
}

#[cfg(test)]
mod ground_gap_tuning {
    use super::*;
    use avian3d::prelude::{AngularInertia, Mass, SubstepCount};
    use avian3d::PhysicsPlugins;
    use bevy::time::TimeUpdateStrategy;
    use core::time::Duration;

    const TIMESTEP: f32 = 1.0 / 60.0;

    use crate::map::bowl_collider as bowl;

    /// Clamp `collider` (resting at `rest_y`) to the real bowl via `part_gap_contacts`,
    /// then run the sim (with the weld census) and return `(weld count, rigid?, peak
    /// resting |v|/|ω|)`. This is the tuning rig for `MIN_JOINT_DIST`.
    fn clamp_to_bowl(collider: Collider, rest_y: f32) -> (usize, bool, f32) {
        let ground_col = bowl();
        let part_pos = Vec3::new(0.0, rest_y, 0.0);
        let mut welds = Vec::new();
        part_gap_contacts(
            &collider,
            part_pos,
            Quat::IDENTITY,
            &ground_col,
            Vec3::ZERO,
            Quat::IDENTITY,
            &mut welds,
        );
        let n = welds.len();
        let rigid = anchors_are_rigid(welds.iter().map(|(a, _)| *a));

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, TransformPlugin, PhysicsPlugins::default()));
        app.insert_resource(Gravity(Vec3::NEG_Y * 9.81));
        app.insert_resource(SubstepCount(6));
        app.insert_resource(Time::<Fixed>::from_duration(Duration::from_secs_f32(TIMESTEP)));
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(TIMESTEP)));
        app.add_systems(FixedUpdate, maintain_weld_rigidity);
        let ground = app
            .world_mut()
            .spawn((RigidBody::Static, ground_col, Position(Vec3::ZERO), Friction::new(0.6), Restitution::new(0.0)))
            .id();
        let part = app
            .world_mut()
            .spawn((
                RigidBody::Dynamic,
                collider,
                ColliderDensity(1.0),
                Mass(1.0),
                AngularInertia::new(Vec3::splat(1.0)),
                Position(part_pos),
                Rotation::default(),
                LinearVelocity::default(),
                AngularVelocity::default(),
                Friction::new(0.6),
                Restitution::new(0.0),
            ))
            .id();
        for (pa, ga) in &welds {
            app.world_mut()
                .spawn(SphericalJoint::new(part, ground).with_local_anchor1(*pa).with_local_anchor2(*ga));
        }
        app.finish();
        const WARMUP: usize = 600;
        const MEASURE: usize = 300;
        let mut peak = 0.0f32;
        for tick in 0..(WARMUP + MEASURE) {
            app.update();
            if tick >= WARMUP {
                let lin = app.world().get::<LinearVelocity>(part).unwrap().0.length();
                let ang = app.world().get::<AngularVelocity>(part).unwrap().0.length();
                peak = peak.max(lin).max(ang);
            }
        }
        (n, rigid, peak)
    }

    fn rocket() -> Collider {
        Collider::compound(vec![
            (Vec3::ZERO, Quat::IDENTITY, Collider::cylinder(ROCKET_BODY_RADIUS, ROCKET_BODY_HEIGHT)),
            (
                Vec3::new(0.0, ROCKET_FLARE_Y_OFFSET, 0.0),
                Quat::IDENTITY,
                Collider::cone(ROCKET_FLARE_BOTTOM_RADIUS, ROCKET_FLARE_HEIGHT),
            ),
        ])
    }

    /// A platform (wide thin cuboid) resting on a rocket's top (cylinder ⌀0.8). The
    /// user wants this to weld with 4 joints, so `MIN_JOINT_DIST` must keep 4 points
    /// on that 0.8-diameter circle. Prints the weld count for the current tuning.
    #[test]
    fn rocket_top_weld_count() {
        let rocket = rocket();
        let platform = Collider::cuboid(3.0, 0.36, 2.4); // TVCDUO-deck-ish, full extents
        // Rocket body top is at y = ROCKET_BODY_HEIGHT/2 = 0.9; platform bottom sits there.
        let plat_pos = Vec3::new(0.0, ROCKET_BODY_HEIGHT / 2.0 + 0.18, 0.0);
        let mut out = Vec::new();
        part_gap_contacts(&rocket, Vec3::ZERO, Quat::IDENTITY, &platform, plat_pos, Quat::IDENTITY, &mut out);
        println!("[rocket-top] MIN_JOINT_DIST={MIN_JOINT_DIST} -> {} welds on rocket top", out.len());
        for (a, _) in &out {
            println!("    weld at rocket-local {:?}", a.to_array().map(|v| (v * 100.0).round() / 100.0));
        }
        // A platform on a rocket's top must get 4 welds (a rigid mount), not a hinge —
        // this is what `MIN_JOINT_DIST` is tuned for.
        assert!(out.len() >= 4, "rocket-top mount must form 4 welds, got {}", out.len());
    }

    #[test]
    fn tune_ground_clamp() {
        // Cuboid resting on the near-flat bowl centre (bowl bottom ≈ y = -1.5).
        let (n1, r1, p1) = clamp_to_bowl(Collider::cuboid(1.0, 1.0, 1.0), -1.0);
        println!("[tune] cuboid: {n1} welds, rigid={r1}, peak |v| = {p1:.5} m/s");

        // Rocket resting on its wide flare base.
        let rocket = Collider::compound(vec![
            (Vec3::ZERO, Quat::IDENTITY, Collider::cylinder(ROCKET_BODY_RADIUS, ROCKET_BODY_HEIGHT)),
            (
                Vec3::new(0.0, ROCKET_FLARE_Y_OFFSET, 0.0),
                Quat::IDENTITY,
                Collider::cone(ROCKET_FLARE_BOTTOM_RADIUS, ROCKET_FLARE_HEIGHT),
            ),
        ]);
        // Flare bottom sits ~ y = -1.5 → body centre at -1.5 + (BODY_H/2 + FLARE_H).
        let rest = -1.5 + ROCKET_BODY_HEIGHT / 2.0 + ROCKET_FLARE_HEIGHT;
        let (n2, r2, p2) = clamp_to_bowl(rocket, rest);
        println!("[tune] rocket: {n2} welds, rigid={r2}, peak |v| = {p2:.5} m/s");

        assert!(r1, "cuboid ground clamp must be rigid (3+ non-collinear welds)");
        assert!(p1 < 0.05, "cuboid ground clamp must settle, peak = {p1}");
        assert!(r2, "rocket ground clamp must be rigid");
        assert!(p2 < 0.05, "rocket ground clamp must settle, peak = {p2}");
    }
}

#[cfg(test)]
mod gap_tests {
    use super::*;

    /// Two axis-aligned unit cubes separated by a small gap along X must produce a
    /// full 4-corner manifold, and every weld must have ZERO rest error — both anchors
    /// map to the same world point now — so the parts stay at their fixed separation
    /// instead of being yanked together.
    #[test]
    fn gap_weld_is_rigid_and_zero_rest_error() {
        let gap = 0.05; // within JOINT_GAP
        let cube = Collider::cuboid(1.0, 1.0, 1.0); // full extents → faces at ±0.5
        let a_pos = Vec3::ZERO;
        let b_pos = Vec3::new(1.0 + gap, 0.0, 0.0);
        let mut out = Vec::new();
        part_gap_contacts(&cube, a_pos, Quat::IDENTITY, &cube, b_pos, Quat::IDENTITY, &mut out);

        assert!(out.len() >= 3, "a flush near-touch must form a rigid (3+) weld");
        for (a_local, b_local) in &out {
            // The weld starts satisfied: a's anchor and b's anchor are the same world
            // point (the gap midpoint at x = 0.5 + gap/2), so nothing is pulled.
            let a_world = a_pos + *a_local;
            let b_world = b_pos + *b_local;
            assert!((a_world - b_world).length() < 1e-4, "zero rest error: {a_world:?} vs {b_world:?}");
            assert!((a_world.x - (0.5 + gap * 0.5)).abs() < 1e-3, "anchor at gap midpoint: {a_world:?}");
        }
    }

    /// An angled approach still welds (zero rest error means it's launch-safe at any
    /// angle) — the parts are simply frozen at their current relative pose.
    #[test]
    fn angled_gap_still_welds_with_zero_rest_error() {
        let cube = Collider::cuboid(1.0, 1.0, 1.0);
        let tilt = Quat::from_rotation_z(8.0_f32.to_radians());
        let a_pos = Vec3::ZERO;
        let b_pos = Vec3::new(1.0 + 0.05, 0.0, 0.0);
        let mut out = Vec::new();
        part_gap_contacts(&cube, a_pos, Quat::IDENTITY, &cube, b_pos, tilt, &mut out);
        assert!(!out.is_empty(), "an angled near-touch still welds");
        for (a_local, b_local) in &out {
            let a_world = a_pos + *a_local;
            let b_world = b_pos + tilt * *b_local;
            assert!((a_world - b_world).length() < 1e-4, "zero rest error even when angled");
        }
    }

    /// Beyond the gap, no weld forms.
    #[test]
    fn no_weld_past_the_gap() {
        let cube = Collider::cuboid(1.0, 1.0, 1.0);
        let mut out = Vec::new();
        part_gap_contacts(
            &cube,
            Vec3::ZERO,
            Quat::IDENTITY,
            &cube,
            Vec3::new(1.0 + JOINT_GAP + 0.05, 0.0, 0.0),
            Quat::IDENTITY,
            &mut out,
        );
        assert!(out.is_empty(), "faces farther apart than JOINT_GAP must not weld");
    }
}

#[cfg(test)]
mod weld_tests {
    use super::*;

    /// 1 point = ball pivot, 2 points = hinge, 3 collinear = still a hinge — all
    /// keep their contact. Only a spanning triangle counts as a rigid weld.
    #[test]
    fn rigidity_census_classifies_welds() {
        let p = |x: f32, y: f32, z: f32| Vec3::new(x, y, z);
        assert!(!anchors_are_rigid([p(0.0, 0.0, 0.0)].into_iter()));
        assert!(!anchors_are_rigid([p(0.0, 0.0, 0.0), p(1.0, 0.0, 0.0)].into_iter()));
        assert!(!anchors_are_rigid(
            [p(0.0, 0.0, 0.0), p(1.0, 0.0, 0.0), p(2.0, 0.0, 0.0)].into_iter()
        ));
        assert!(anchors_are_rigid(
            [p(0.0, 0.0, 0.0), p(1.0, 0.0, 0.0), p(0.0, 0.0, 1.0)].into_iter()
        ));
        // A face weld (4 corner points) is rigid.
        assert!(anchors_are_rigid(
            [p(-0.5, 0.0, -0.5), p(0.5, 0.0, -0.5), p(0.5, 0.0, 0.5), p(-0.5, 0.0, 0.5)]
                .into_iter()
        ));
    }

    /// The census is cluster-aware: a body bolted by SINGLE joints to several members
    /// of one rigid cluster — whose anchors on the body span a triangle — is rigid to
    /// the cluster, so all of those hinge joints drop their contact (the "6 rocks"
    /// rigid-loop-of-hinges case). A genuinely dangling hinge keeps its contact.
    #[test]
    fn rigid_loop_of_hinges_disables_contact() {
        let mut app = App::new();
        // `JointCollisionDisabled`'s component hooks write into avian's `JointGraph`
        // resource (normally added by `PhysicsPlugins`); the census only needs it to exist.
        app.init_resource::<avian3d::dynamics::solver::joint_graph::JointGraph>();
        app.add_systems(Update, maintain_weld_rigidity);
        let world = app.world_mut();
        let p = |x: f32, y: f32, z: f32| Vec3::new(x, y, z);
        let deck = world.spawn_empty().id();
        let r1 = world.spawn_empty().id();
        let r2 = world.spawn_empty().id();
        let bolt = world.spawn_empty().id();
        let dangler = world.spawn_empty().id();
        let weld = |world: &mut World, a: Entity, b: Entity, anchors: [Vec3; 3]| {
            for anchor in anchors {
                world.spawn(
                    SphericalJoint::new(a, b)
                        .with_local_anchor1(anchor)
                        .with_local_anchor2(anchor + Vec3::X),
                );
            }
        };
        // Rigid cluster: deck↔r1 and deck↔r2 (3 non-collinear anchors each).
        let tri = [p(0.0, 0.0, 0.0), p(1.0, 0.0, 0.0), p(0.0, 0.0, 1.0)];
        weld(world, deck, r1, tri);
        weld(world, deck, r2, tri);
        // The bolt-on: ONE joint to each of deck/r1/r2; its own-side (body1) anchors
        // span a triangle only in aggregate — every pair alone is a hinge.
        let loop_joints: Vec<Entity> = [(deck, p(0.0, 0.0, 0.0)), (r1, p(1.0, 0.0, 0.0)), (r2, p(0.0, 0.0, 1.0))]
            .into_iter()
            .map(|(other, anchor)| {
                world
                    .spawn(
                        SphericalJoint::new(bolt, other)
                            .with_local_anchor1(anchor)
                            .with_local_anchor2(anchor + Vec3::Y),
                    )
                    .id()
            })
            .collect();
        // A dangling hinge: one joint to the deck, nothing else.
        let hinge = world
            .spawn(SphericalJoint::new(dangler, deck).with_local_anchor1(Vec3::ZERO))
            .id();
        app.update();
        let world = app.world();
        for joint in &loop_joints {
            assert!(
                world.get::<JointCollisionDisabled>(*joint).is_some(),
                "loop hinge joint should drop contact (rigid via the cluster)"
            );
        }
        assert!(
            world.get::<JointCollisionDisabled>(hinge).is_none(),
            "dangling hinge must keep its contact (it braces the part)"
        );
    }
}
