use crate::map::PLATFORM_WIDTH_M;
use crate::player::get_hold_point_entity;
use crate::utils::{self, QuatExt, Vec3Ext};
use crate::{
    AttachEvent, Attachable, BoundingRadius, CameraOrbitCenter, DisplayableJoint, ExistingJoints,
    Focused, FocusedInteractable, HoldPoint, Holding, Modifying, Player, PlayerClick,
    PotentialJoints, PredeleteJoint, PredeleteJoints, ToggleHoldingSystemLabel, UpdateJointsLabel,
};
use bevy::prelude::*;
use bevy_rapier3d::plugin::{RapierConfiguration, RapierContext};
use bevy_rapier3d::prelude::{
    ActiveEvents, Collider, ColliderMassProperties, ExternalForce, Friction, ImpulseJoint,
    ReadMassProperties, Restitution, RigidBody, SphericalJointBuilder, Velocity,
};
use bevy_rapier3d::rapier::prelude::ColliderShape;
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
                    position_held_part.after(zero_part_external_forces),
                    orient_held_part.after(zero_part_external_forces),
                    spawn_part,
                    update_attachable,
                    zero_part_external_forces,
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
            .add_event::<NewPart>()
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
    mass_properties: ReadMassProperties,
    velocity: Velocity,
    external_force: ExternalForce,
}

#[derive(Event)]
struct NewPart;

fn get_random_shape(rng: &mut ThreadRng) -> ColliderShape {
    loop {
        let (x, y, z) = (
            rng.gen_range(MIN_PART_SIZE..=MAX_PART_SIZE),
            rng.gen_range(MIN_PART_SIZE..=MAX_PART_SIZE),
            rng.gen_range(MIN_PART_SIZE..=MAX_PART_SIZE),
        );
        let volume = x * y * z;
        if volume < MAX_PART_VOLUME && volume > MIN_PART_VOLUME {
            return ColliderShape::cuboid(x / 2.0, y / 2.0, z / 2.0);
        }
    }
}

fn spawn_part(mut commands: Commands, mut new_part_events: EventReader<NewPart>) {
    let mut rng = rand::thread_rng();
    for _ in new_part_events.read() {
        let shape = get_random_shape(&mut rng);
        commands
            .spawn_empty()
            .insert(BoundingRadius(shape.compute_local_bounding_sphere().radius))
            .insert(RigidBody::Dynamic)
            .insert(TransformBundle::from(Transform::from_xyz(
                rng.gen_range(-SPAWN_ZONE_HALF_WIDTH..=SPAWN_ZONE_HALF_WIDTH),
                rng.gen_range(5.0..=15.0),
                rng.gen_range(-SPAWN_ZONE_HALF_WIDTH..=SPAWN_ZONE_HALF_WIDTH),
            )))
            .insert(Collider::from(shape))
            .insert(ColliderMassProperties::Density(2.0))
            .insert(Friction::coefficient(1.0))
            .insert(Restitution::coefficient(0.1))
            .insert(ActiveEvents::COLLISION_EVENTS)
            .insert(PartBundle::default());
    }
}

fn spawn_initial_parts(mut new_part_events: EventWriter<NewPart>) {
    for _ in 0..NUM_PARTS {
        new_part_events.send(NewPart);
    }
}

fn replace_fallen_parts(
    mut commands: Commands,
    parts: Query<(&Transform, Entity), With<GetsReplaced>>,
    mut new_part_events: EventWriter<NewPart>,
) {
    for (transform, entity) in parts.iter() {
        if transform.translation.y < -10.0 {
            commands.entity(entity).despawn();
            new_part_events.send(NewPart);
        }
    }
}

const MAX_INTERACT_DISTANCE: f32 = 7.5;
const MAX_INTERACT_DISTANCE_SQUARED: f32 = MAX_INTERACT_DISTANCE * MAX_INTERACT_DISTANCE;
const MAX_INTERACT_ANGLE_DEGREES: f32 = 20.0;
const MAX_INTERACT_ANGLE: f32 = MAX_INTERACT_ANGLE_DEGREES * utils::DEG_TO_RADIANS;

fn update_focused(
    mut commands: Commands,
    mut players: Query<(&mut FocusedInteractable, &Holding, &Children, &Modifying), With<Player>>,
    mut interactables: Query<(&mut Transform, Entity), With<Interactable>>,
    camera_orbit_centers: Query<&GlobalTransform, With<CameraOrbitCenter>>,
) {
    // Determine which iteractable entity each player is focused on (i.e. looking at, within range)
    for (mut focused_interactable, holding, player_children, modifying) in players.iter_mut() {
        if !holding.0 {
            let mut newly_focused_interactable_option = None;
            if !modifying.0 {
                for player_child in player_children.iter() {
                    if let Ok(camera_orbit_center) = camera_orbit_centers.get(*player_child) {
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
    rapier_context: Res<RapierContext>,
) {
    if let Some(held) = helds.iter().next() {
        let contacted = rapier_context
            .contact_pairs_with(held)
            .filter(|x| x.has_any_active_contacts())
            .map(|contact_pair| {
                if contact_pair.collider1() == held {
                    contact_pair.collider2()
                } else {
                    contact_pair.collider1()
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
    mut parts: Query<(
        &Transform,
        &TargetPosition,
        &ReadMassProperties,
        &Velocity,
        &mut ExternalForce,
    )>,
    physics_config: Res<RapierConfiguration>,
) {
    for (part_transform, target_position, mass_properties, velocity, mut ext_forces) in
        parts.iter_mut()
    {
        if let Ok(hold_point_position) = hold_points.get(target_position.hold_point_entity) {
            let vector_between = hold_point_position.translation() - part_transform.translation;
            let positioning_acceleration = target_position
                .oscillator
                .calculate_acceleration(&vector_between.into(), &velocity.linvel);

            // bevy_rapier 0.23 made `ReadMassProperties`'s inner field private;
            // it derefs to `MassProperties`, so drop the `.0`.
            let gravity_cancelation_force = -mass_properties.mass * physics_config.gravity;
            let positioning_force = positioning_acceleration * mass_properties.mass;
            ext_forces.force = positioning_force + gravity_cancelation_force;
        }
    }
}

fn zero_part_external_forces(mut parts: Query<&mut ExternalForce, With<Holdable>>) {
    for mut forces in parts.iter_mut() {
        forces.force = Vec3::ZERO;
        forces.torque = Vec3::ZERO;
    }
}

fn orient_held_part(
    mut parts: Query<(
        &Transform,
        &TargetOrientation,
        &Velocity,
        &mut ExternalForce,
        &ReadMassProperties,
    )>,
) {
    for (part_transform, target_orientation, velocity, mut ext_forces, mass_properties) in
        parts.iter_mut()
    {
        let rotation_between =
            (target_orientation.quat * part_transform.rotation.conjugate()).to_rotation_vector();
        let angular_acceleration = target_orientation
            .oscillator
            .calculate_acceleration(&rotation_between, &velocity.angvel);
        let inertia_sqrt = mass_properties.principal_inertia_local_frame;
        // let torque = inertia_sqrt * (inertia_sqrt * angular_acceleration);
        let torque = inertia_sqrt * angular_acceleration;
        ext_forces.torque = torque;
    }
}

fn update_active_joints(
    holdables: Query<Option<&Children>, (With<GlobalTransform>, With<Holdable>)>, //remove transform??
    rapier_context: Res<RapierContext>,
    mut potential_joints: ResMut<PotentialJoints>,
    mut existing_joints: ResMut<ExistingJoints>,
    players: Query<(&Holding, &FocusedInteractable)>,
    joints: Query<(&Parent, &ImpulseJoint)>,
) {
    potential_joints.0.clear();
    existing_joints.0.clear();

    if let Some((holding, interactable)) = players.iter().next() {
        if holding.0 {
            if let Some(held_entity) = interactable.0 {
                for contact_pair in rapier_context.contact_pairs_with(held_entity) {
                    let attachable_entity = if contact_pair.collider1() == held_entity {
                        contact_pair.collider2()
                    } else {
                        contact_pair.collider1()
                    };

                    for (parent, joint) in joints.iter() {
                        if parent.get() == held_entity && joint.parent == attachable_entity {
                            existing_joints.0.push(DisplayableJoint {
                                entities: (held_entity, attachable_entity),
                                points: (
                                    joint.data.raw.local_frame2.translation.vector.into(),
                                    joint.data.raw.local_frame1.translation.vector.into(), // todo: or just local anchor?
                                ),
                            });
                        } else if parent.get() == attachable_entity && joint.parent == held_entity {
                            existing_joints.0.push(DisplayableJoint {
                                entities: (attachable_entity, held_entity),
                                points: (
                                    joint.data.raw.local_frame2.translation.vector.into(),
                                    joint.data.raw.local_frame1.translation.vector.into(), // todo: or just local anchor?
                                ),
                            });
                        }
                    }

                    if contact_pair.has_any_active_contacts() {
                        for manifold in contact_pair.manifolds() {
                            for contact in manifold.points() {
                                if existing_joints
                                    .0
                                    .iter()
                                    .map(|p| (p.points.0 - contact.local_p1()).norm())
                                    .all(|d| d > MIN_JOINT_SPACING)
                                {
                                    potential_joints.0.push(DisplayableJoint {
                                        entities: (
                                            contact_pair.collider1(),
                                            contact_pair.collider2(),
                                        ),
                                        points: (contact.local_p1(), contact.local_p2()),
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
    joints: Query<(Entity, &ImpulseJoint, &Parent)>,
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
                    for (joint_entity, joint, joint_parent) in joints.iter() {
                        if let Ok(transform) = holdables.get(joint_parent.get()) {
                            let transform = transform.compute_transform();
                            let center = transform.translation
                                + transform.rotation.mul_vec3(
                                    joint.data.raw.local_frame2.translation.vector.into(),
                                );
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
    mut attach_events: EventReader<AttachEvent>,
    attach_points: Res<PotentialJoints>,
) {
    if attach_events.read().next().is_some() {
        for DisplayableJoint { points, entities } in attach_points.0.iter() {
            let joint = SphericalJointBuilder::new()
                .local_anchor1(points.1)
                .local_anchor2(points.0);
            commands.entity(entities.0).with_children(|children| {
                children
                    .spawn_empty()
                    .insert(ImpulseJoint::new(entities.1, joint));
            });
        }
    }
}

fn delete_joints(
    mut commands: Commands,
    predelete_joints: Res<PredeleteJoints>,
    mut clicks: EventReader<PlayerClick>,
) {
    if clicks.read().next().is_some() {
        for PredeleteJoint { entity, .. } in predelete_joints.0.iter() {
            commands.entity(*entity).despawn_recursive();
        }
    }
}
