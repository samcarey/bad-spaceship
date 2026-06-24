use crate::map::PLATFORM_WIDTH_M;
use crate::player::get_hold_point_entity;
use crate::utils::{self, QuatExt, Vec3Ext};
use crate::{
    AttachEvent, Attachable, BoundingRadius, CameraOrbitCenter, DisplayableJoint, ExistingJoints,
    Focused, FocusedInteractable, HoldPoint, Holding, Modifying, Player, PlayerClick,
    PotentialJoints, PredeleteJoint, PredeleteJoints, ToggleHoldingSystemLabel, UpdateJointsLabel,
};
use avian3d::prelude::{
    AngularVelocity, Collider, ColliderDensity, Collisions, ComputedCenterOfMass, Forces, Friction,
    Gravity, LinearVelocity, ReadRigidBodyForces, Restitution, RigidBody, SphericalJoint, SweptCcd,
    WriteRigidBodyForces,
};
use bevy::prelude::*;
use rand::prelude::ThreadRng;
use rand::Rng;
use std::f32;

pub struct PartPlugin;

impl Plugin for PartPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_initial_parts)
            .add_systems(
                Update,
                (
                    replace_fallen_parts,
                    update_focused,
                    // Avian's `Forces` helper auto-clears after the physics step, so
                    // the old per-frame `zero_part_external_forces` system is gone.
                    // Both force systems write through `Forces` on the held part, so
                    // order them to avoid an ambiguous double-write.
                    position_held_part,
                    orient_held_part.after(position_held_part),
                    spawn_part,
                    update_attachable,
                    (update_active_joints, update_predelete_joints)
                        .in_set(UpdateJointsLabel)
                        .before(ToggleHoldingSystemLabel),
                    attach
                        .after(ToggleHoldingSystemLabel)
                        .after(UpdateJointsLabel),
                    delete_joints
                        .after(ToggleHoldingSystemLabel)
                        .after(UpdateJointsLabel),
                ),
            )
            .add_message::<NewPart>()
            .init_resource::<PotentialJoints>()
            .init_resource::<ExistingJoints>()
            .init_resource::<PredeleteJoints>();
    }
}

const NUM_PARTS: i32 = 10;
pub const MAX_PART_SIZE: f32 = 10.0;
const MIN_PART_SIZE: f32 = 0.1;
const MIN_PART_VOLUME: f32 = 1.0;
const MAX_PART_VOLUME: f32 = 2.0;
const POSITIONING_STIFFNESS: f32 = 30.0;
const ORIENTING_STIFFNESS: f32 = 5.0;
const MIN_JOINT_SPACING: f32 = MIN_PART_SIZE / 2.0;
pub const DELETE_RADIUS: f32 = 1.0;

#[derive(Default, Component)]
struct Interactable;

#[derive(Default, Component)]
pub struct Holdable;

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
    let mut rng = rand::thread_rng();
    for _ in new_part_events.read() {
        let collider = get_random_shape(&mut rng);
        // Bounding radius from the parry shape, before the collider is moved in.
        let bounding_radius = collider.shape().compute_local_bounding_sphere().radius;
        commands
            .spawn_empty()
            .insert(BoundingRadius(bounding_radius))
            .insert(RigidBody::Dynamic)
            // Bevy 0.15: bare `Transform` (it now requires `GlobalTransform`).
            .insert(Transform::from_xyz(
                rng.gen_range(-SPAWN_ZONE_HALF_WIDTH..=SPAWN_ZONE_HALF_WIDTH),
                rng.gen_range(5.0..=15.0),
                rng.gen_range(-SPAWN_ZONE_HALF_WIDTH..=SPAWN_ZONE_HALF_WIDTH),
            ))
            .insert(collider)
            // rapier's `ColliderMassProperties::Density` / `Friction::coefficient` /
            // `Restitution::coefficient` → Avian's `ColliderDensity` / `Friction::new`
            // / `Restitution::new`. Avian tracks contacts in its graph by default, so
            // rapier's `ActiveEvents::COLLISION_EVENTS` opt-in is no longer needed.
            .insert(ColliderDensity(2.0))
            .insert(Friction::new(1.0))
            .insert(Restitution::new(0.1))
            // Blocks spawn high (y 5..15) and hit the thin trimesh ground fast.
            // Without continuous collision detection a fast impact can penetrate
            // deeply in a single solver step and the soft-contact recovery leaves
            // the block partially embedded. CCD catches the fast impact so blocks
            // rest flush. (rapier's `Ccd::enabled()` → Avian's `SweptCcd`.)
            .insert(SweptCcd::default())
            .insert(PartBundle::default());
    }
}

fn spawn_initial_parts(mut new_part_events: MessageWriter<NewPart>) {
    for _ in 0..NUM_PARTS {
        new_part_events.write(NewPart);
    }
}

fn replace_fallen_parts(
    mut commands: Commands,
    parts: Query<(&Transform, Entity), With<GetsReplaced>>,
    mut new_part_events: MessageWriter<NewPart>,
) {
    for (transform, entity) in parts.iter() {
        if transform.translation.y < -10.0 {
            commands.entity(entity).despawn();
            new_part_events.write(NewPart);
        }
    }
}

const MAX_INTERACT_DISTANCE: f32 = 7.5;
const MAX_INTERACT_DISTANCE_SQUARED: f32 = MAX_INTERACT_DISTANCE * MAX_INTERACT_DISTANCE;
const MAX_INTERACT_ANGLE_DEGREES: f32 = 20.0;
const MAX_INTERACT_ANGLE: f32 = MAX_INTERACT_ANGLE_DEGREES * utils::DEG_TO_RADIANS;

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
                        // Avian contact anchors are world-space, relative to each
                        // body's center of mass: `anchor = world_point - (pos + rot *
                        // com_local)`. Recover the body-local contact point with
                        // `rot⁻¹ * anchor + com_local`. Dropping the `+ com_local`
                        // term only happens to work when the COM sits at the origin
                        // (the centered cuboid parts); the ground trimesh's COM does
                        // not, so omitting it offset the ground anchor and dragged the
                        // joined part down into the bowl.
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
                                let local_p1 = rot1.inverse() * contact.anchor1 + com1;
                                let local_p2 = rot2.inverse() * contact.anchor2 + com2;
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
    players: Query<(&Holding, &Modifying, &Children)>,
    joints: Query<(Entity, &SphericalJoint)>,
    hold_points0: Query<(), With<HoldPoint>>,
    hold_points1: Query<&GlobalTransform, With<HoldPoint>>,
    camera_orbit_centers: Query<&Children>,
) {
    predelete_joints.0.clear();

    if let Some((holding, modifying, player_children)) = players.iter().next() {
        if !holding.0 && modifying.0 {
            if let Some(entity) =
                get_hold_point_entity(player_children, camera_orbit_centers, &hold_points0)
            {
                if let Ok(hold_point_position) = hold_points1.get(entity) {
                    for (joint_entity, joint) in joints.iter() {
                        // World position of the joint's anchor on `body2` (rapier
                        // used the joint's parent body + `local_frame2`).
                        if let (Ok(transform), Some(anchor2)) =
                            (holdables.get(joint.body2), joint.local_anchor2())
                        {
                            let transform = transform.compute_transform();
                            let center =
                                transform.translation + transform.rotation.mul_vec3(anchor2);
                            if (center - hold_point_position.translation()).length() < DELETE_RADIUS
                            {
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
}

fn attach(
    mut commands: Commands,
    mut attach_events: MessageReader<AttachEvent>,
    attach_points: Res<PotentialJoints>,
) {
    if attach_events.read().next().is_some() {
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
