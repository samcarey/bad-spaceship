use crate::map::PLATFORM_WIDTH_M;
use crate::utils::{self, QuatExt, Vec3Ext};
use crate::{
    AttachEvent, Attachable, BoundingRadius, CameraOrbitCenter, DisplayableJoint, ExistingJoints,
    Focused, FocusedInteractable, HoldPoint, Holding, Modifying, Player, PlayerClick, PlayerHoldPoint,
    PotentialJoints, PredeleteJoint, PredeleteJoints, ToggleHoldingSystemLabel, UpdateJointsLabel,
};
use avian3d::prelude::{
    AngularVelocity, Collider, ColliderDensity, Collisions, ComputedCenterOfMass, Forces, Friction,
    Gravity, LinearVelocity, Position, ReadRigidBodyForces, Restitution, RigidBody, SphericalJoint,
    SweptCcd, WriteRigidBodyForces,
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
            .add_systems(
                Update,
                (
                    replace_fallen_parts.run_if(not(resource_exists::<SuppressLocalParts>)),
                    replace_fallen_rocket_engines
                        .run_if(not(resource_exists::<SuppressLocalParts>)),
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
// Held-part spring stiffnesses — also used by the multiplayer hold helpers in
// `net.rs` (the server runs the same critically-damped springs), so they're
// `pub` to keep a single definition rather than duplicated magic numbers.
pub const POSITIONING_STIFFNESS: f32 = 30.0;
pub const ORIENTING_STIFFNESS: f32 = 5.0;
const MIN_JOINT_SPACING: f32 = MIN_PART_SIZE / 2.0;
pub const DELETE_RADIUS: f32 = 1.0;

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

/// Below this Y a part/rocket has fallen off the platform and is recycled.
const PART_FALL_Y: f32 = -10.0;

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
        ColliderDensity(2.0),
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
    let mut entity = commands.spawn((
        RocketEngine,
        Interactable,
        Holdable,
        collider,
        BoundingRadius(bounding_radius),
        LinearVelocity::default(),
        AngularVelocity::default(),
        // Seed both `Transform` and Avian `Position` (see `spawn_random_part`).
        Transform::from_translation(position),
        Position(position),
    ));
    // Density/friction/restitution/CCD/rigid-body — shared with the cuboid parts.
    insert_part_dynamics(&mut entity);
    entity.id()
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

/// A rocket engine that falls off the platform is despawned and a fresh one
/// dropped back in — the rocket-engine counterpart to `replace_fallen_parts`
/// (rockets carry no `GetsReplaced`, since that path respawns a *cuboid*).
fn replace_fallen_rocket_engines(
    mut commands: Commands,
    rockets: Query<(&Transform, Entity), With<RocketEngine>>,
) {
    let mut rng = rand::thread_rng();
    for (transform, entity) in rockets.iter() {
        if transform.translation.y < PART_FALL_Y {
            commands.entity(entity).despawn();
            spawn_rocket_engine(&mut commands, random_rocket_spawn(&mut rng));
        }
    }
}

fn replace_fallen_parts(
    mut commands: Commands,
    parts: Query<(&Transform, Entity), With<GetsReplaced>>,
    mut new_part_events: MessageWriter<NewPart>,
) {
    for (transform, entity) in parts.iter() {
        if transform.translation.y < PART_FALL_Y {
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
    mut commands: Commands,
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

            let mut interactable_to_unfocus = None;
            if let Some(newly_focused_interactable) = newly_focused_interactable_option {
                let mut interactable_to_focus = Some(newly_focused_interactable);
                if let Some(previously_focused_interactable) = focused_interactable.0 {
                    if newly_focused_interactable == previously_focused_interactable {
                        interactable_to_focus = None;
                    } else {
                        interactable_to_unfocus = Some(previously_focused_interactable);
                    }
                }
                if let Some(entity) = interactable_to_focus {
                    commands.entity(entity).insert(Focused);
                }

                focused_interactable.0 = Some(newly_focused_interactable);
            } else {
                if let Some(previous_focused_interactable) = focused_interactable.0 {
                    interactable_to_unfocus = Some(previous_focused_interactable);
                }
                focused_interactable.0 = None;
            }

            if let Some(interactable) = interactable_to_unfocus {
                commands.entity(interactable).remove::<Focused>();
            }
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

/// Recover a contact point in a body's local frame from Avian's contact anchor.
///
/// Avian reports contact anchors in **world orientation, relative to each body's
/// center of mass**: `anchor = world_point - (pos + rot * com_local)`. The joint
/// builders want a **body-local** anchor, recovered as `rot⁻¹ * anchor + com_local`.
/// The `+ com_local` term matters: dropping it only happens to work when the COM
/// sits at the origin (the centered cuboid parts), but the ground trimesh's COM does
/// not — omitting it there offset the ground anchor and dragged the joined part down
/// into the bowl. Shared by single-player `update_active_joints` and the server's
/// `server_attach` so the anchor convention stays in one place.
pub fn local_contact_anchor(rotation: Quat, com: Vec3, anchor: Vec3) -> Vec3 {
    rotation.inverse() * anchor + com
}

fn update_active_joints(
    collisions: Collisions,
    // Body rotations + centers of mass, used to map Avian's world-space,
    // COM-relative contact anchors into each body's local frame (see the per-point
    // conversion below). The cuboid parts have their COM at the origin, but the
    // ground bowl is a trimesh whose COM is *not* at its origin — so the COM term
    // is required, otherwise a part joined to the ground gets yanked into it.
    transforms: Query<&Transform>,
    centers_of_mass: Query<&ComputedCenterOfMass>,
    mut potential_joints: ResMut<PotentialJoints>,
    mut existing_joints: ResMut<ExistingJoints>,
    players: Query<(&Holding, &FocusedInteractable)>,
    // Avian joints are standalone entities carrying `body1`/`body2` (rapier's
    // joint was a child of one body, with the other reached via `joint.parent`).
    joints: Query<&SphericalJoint>,
) {
    potential_joints.0.clear();
    existing_joints.0.clear();

    if let Some((holding, interactable)) = players.iter().next() {
        if holding.0 {
            if let Some(held_entity) = interactable.0 {
                for contact_pair in collisions.collisions_with(held_entity) {
                    // Avian's `collider1/2` are plain `Entity` (rapier's were `Option`).
                    let (collider1, collider2) =
                        (contact_pair.collider1, contact_pair.collider2);
                    let attachable_entity = if collider1 == held_entity {
                        collider2
                    } else {
                        collider1
                    };

                    // The `DisplayableJoint` convention is "points.0 is the local
                    // anchor on entities.0"; `attach` maps body2/anchor2 → entities.0.
                    for joint in joints.iter() {
                        let (Some(anchor1), Some(anchor2)) =
                            (joint.local_anchor1(), joint.local_anchor2())
                        else {
                            continue;
                        };
                        if joint.body2 == held_entity && joint.body1 == attachable_entity {
                            existing_joints.0.push(DisplayableJoint {
                                entities: (held_entity, attachable_entity),
                                points: (anchor2, anchor1),
                            });
                        } else if joint.body2 == attachable_entity && joint.body1 == held_entity {
                            existing_joints.0.push(DisplayableJoint {
                                entities: (attachable_entity, held_entity),
                                points: (anchor2, anchor1),
                            });
                        }
                    }

                    if contact_pair.is_touching() {
                        // Recover each contact point in its body's local frame (see
                        // `local_contact_anchor` for the COM-relative anchor convention).
                        let rot1 = transforms
                            .get(collider1)
                            .map(|t| t.rotation)
                            .unwrap_or(Quat::IDENTITY);
                        let rot2 = transforms
                            .get(collider2)
                            .map(|t| t.rotation)
                            .unwrap_or(Quat::IDENTITY);
                        let com1 = centers_of_mass
                            .get(collider1)
                            .map(|c| c.0)
                            .unwrap_or(Vec3::ZERO);
                        let com2 = centers_of_mass
                            .get(collider2)
                            .map(|c| c.0)
                            .unwrap_or(Vec3::ZERO);
                        for manifold in &contact_pair.manifolds {
                            for contact in &manifold.points {
                                let local_p1 = local_contact_anchor(rot1, com1, contact.anchor1);
                                let local_p2 = local_contact_anchor(rot2, com2, contact.anchor2);
                                if existing_joints
                                    .0
                                    .iter()
                                    .map(|p| (p.points.0 - local_p1).norm())
                                    .all(|d| d > MIN_JOINT_SPACING)
                                {
                                    potential_joints.0.push(DisplayableJoint {
                                        entities: (collider1, collider2),
                                        points: (local_p1, local_p2),
                                    });
                                }
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
    joints: Query<(Entity, &SphericalJoint)>,
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
            // Avian joints are standalone entities referencing both bodies (rapier
            // spawned the joint as a child of `entities.0`). Preserve the rapier
            // anchor mapping: body1/anchor1 = entities.1/points.1, and
            // body2/anchor2 = entities.0/points.0 — which keeps `update_*_joints`
            // and the gizmo rendering (which read back `body2`/`anchor2`) consistent.
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
