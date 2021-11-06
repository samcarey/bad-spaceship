use crate::map::PLATFORM_WIDTH_M;
use crate::player::get_hold_point_entity;
use crate::utils::{self, QuatExt, TransformExt};
use crate::{
    AttachEvent, Attachable, BoundingRadius, CameraOrbitCenter, DisplayableJoint, ExistingJoints,
    Focused, FocusedInteractable, HoldPoint, Holding, Modifying, Player, PlayerClick,
    PotentialJoints, PredeleteJoint, PredeleteJoints, ToggleHoldingSystemLabel,
    UpdateAttachPointsLabel,
};
use bevy::prelude::*;
use bevy_rapier3d::na::Vector3;
use bevy_rapier3d::physics::{
    ColliderBundle, ColliderPositionSync, IntoEntity, IntoHandle, JointBuilderComponent,
    JointHandleComponent, RapierConfiguration, RigidBodyBundle,
};
use bevy_rapier3d::prelude::{
    ActiveEvents, BallJoint, ColliderFlags, ColliderMassProps, ColliderMaterial, ColliderShape,
    JointSet, NarrowPhase, RigidBodyForces, RigidBodyMassProps, RigidBodyVelocity,
};
use bevy_rapier3d::render::ColliderDebugRender;
use rand::prelude::ThreadRng;
use rand::Rng;
use std::f32;

pub struct PartPlugin;

impl Plugin for PartPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_startup_system(spawn_initial_parts.system())
            .add_system(replace_fallen_parts.system())
            .add_system(update_focused.system())
            .add_system(position_held_part.system())
            .add_event::<NewPart>()
            .add_system(orient_held_part.system())
            .add_system(spawn_part.system())
            .add_system(update_attachable.system())
            .add_system_set(
                SystemSet::new()
                    .label(UpdateAttachPointsLabel)
                    .before(ToggleHoldingSystemLabel)
                    .with_system(update_active_joints.system())
                    .with_system(update_predelete_joints.system()),
            )
            .add_system(
                attach
                    .system()
                    .after(ToggleHoldingSystemLabel)
                    .after(UpdateAttachPointsLabel),
            )
            .add_system(
                delete_joints
                    .system()
                    .after(ToggleHoldingSystemLabel)
                    .after(UpdateAttachPointsLabel),
            )
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

#[derive(Default)]
struct Interactable;

#[derive(Default)]
pub struct Holdable;

#[derive(Default)]
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

    pub fn calculate_acceleration(
        &self,
        displacement: &Vector3<f32>,
        velocity: &Vector3<f32>,
    ) -> Vector3<f32> {
        displacement * self.stiffness - velocity * self.damping
    }
}

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
    collider_debug_render: ColliderDebugRender,
    collider_position_sync: ColliderPositionSync,
}

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
    for _ in new_part_events.iter() {
        let shape = get_random_shape(&mut rng);
        commands
            .spawn()
            .insert(BoundingRadius(shape.compute_local_bounding_sphere().radius))
            .insert_bundle(RigidBodyBundle {
                body_type: bevy_rapier3d::prelude::RigidBodyType::Dynamic,
                position: [
                    rng.gen_range(-SPAWN_ZONE_HALF_WIDTH..=SPAWN_ZONE_HALF_WIDTH),
                    rng.gen_range(5.0..=15.0),
                    rng.gen_range(-SPAWN_ZONE_HALF_WIDTH..=SPAWN_ZONE_HALF_WIDTH),
                ]
                .into(),
                ..Default::default()
            })
            .insert_bundle(ColliderBundle {
                shape,
                mass_properties: ColliderMassProps::Density(2.0),
                material: ColliderMaterial {
                    friction: 1.0,
                    restitution: 0.1,
                    ..Default::default()
                },
                flags: ColliderFlags {
                    active_events: ActiveEvents::CONTACT_EVENTS,
                    ..Default::default()
                },
                ..Default::default()
            })
            .insert_bundle(PartBundle {
                collider_debug_render: ColliderDebugRender::with_id(1),
                ..Default::default()
            });
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
                                - camera_orbit_center.translation;
                            if vector_between.length_squared() < MAX_INTERACT_DISTANCE_SQUARED {
                                let angle_from_look =
                                    camera_orbit_center.forward().angle_between(vector_between);

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
    narrow_phase: Res<NarrowPhase>,
) {
    if let Some(held) = helds.iter().next() {
        let contacted = narrow_phase
            .contacts_with(held.handle())
            .filter(|x| x.has_any_active_contact)
            .map(|contact_pair| {
                if contact_pair.collider1.entity() == held {
                    contact_pair.collider2.entity()
                } else {
                    contact_pair.collider1.entity()
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
        &RigidBodyMassProps,
        &RigidBodyVelocity,
        &mut RigidBodyForces,
    )>,
    physics_config: Res<RapierConfiguration>,
) {
    for (part_transform, target_position, mass_properties, velocity, mut forces) in parts.iter_mut()
    {
        if let Ok(hold_point_position) = hold_points.get(target_position.hold_point_entity) {
            let vector_between = hold_point_position.translation - part_transform.translation;
            let positioning_acceleration = target_position
                .oscillator
                .calculate_acceleration(&vector_between.into(), &velocity.linvel);

            let gravity_cancelation_force = -mass_properties.mass() * physics_config.gravity;
            let positioning_force = positioning_acceleration * mass_properties.mass();
            forces.force += positioning_force + gravity_cancelation_force;
        }
    }
}

fn orient_held_part(
    mut parts: Query<(
        &Transform,
        &TargetOrientation,
        &RigidBodyVelocity,
        &mut RigidBodyForces,
        &RigidBodyMassProps,
    )>,
) {
    for (part_transform, target_orientation, velocity, mut forces, mass_properties) in
        parts.iter_mut()
    {
        let rotation_between =
            (target_orientation.quat * part_transform.rotation.conjugate()).to_rotation_vector();
        let angular_acceleration = target_orientation
            .oscillator
            .calculate_acceleration(&rotation_between, &velocity.angvel);
        let inertia_sqrt = mass_properties
            .effective_world_inv_inertia_sqrt
            .inverse_unchecked();
        let torque = inertia_sqrt * (inertia_sqrt * angular_acceleration);
        forces.torque += torque;
    }
}

fn update_active_joints(
    holdables: Query<&GlobalTransform, With<Holdable>>,
    narrow_phase: Res<NarrowPhase>,
    mut potential_joints: ResMut<PotentialJoints>,
    mut existing_joints: ResMut<ExistingJoints>,
    players: Query<(&Holding, &FocusedInteractable)>,
    joint_handles: Query<(Entity, &JointHandleComponent)>,
    joint_set: ResMut<JointSet>,
) {
    potential_joints.0.clear();
    existing_joints.0.clear();

    if let Some((holding, interactable)) = players.iter().next() {
        if holding.0 {
            if let Some(entity1) = interactable.0 {
                for contact_pair in narrow_phase.contacts_with(entity1.handle()) {
                    let entity2 = if contact_pair.collider1.entity() == entity1 {
                        contact_pair.collider2.entity()
                    } else {
                        contact_pair.collider1.entity()
                    };

                    if holdables.get(entity2).is_ok() {
                        for (_, handle) in joint_handles.iter() {
                            if handle.entity1() == entity1 && handle.entity2() == entity2
                                || handle.entity1() == entity2 && handle.entity2() == entity1
                            {
                                if let Some(joint) = joint_set.get(handle.handle()) {
                                    if let Some(ball_joint) = joint.params.as_ball_joint() {
                                        existing_joints.0.push(DisplayableJoint {
                                            entities: (joint.body1.entity(), joint.body2.entity()),
                                            points: (
                                                ball_joint.local_anchor1,
                                                ball_joint.local_anchor2,
                                            ),
                                        });
                                    }
                                }
                            }
                        }
                        if contact_pair.has_any_active_contact {
                            for manifold in &contact_pair.manifolds {
                                for point in &manifold.points {
                                    if existing_joints
                                        .0
                                        .iter()
                                        .map(|p| (p.points.0 - point.local_p1).norm() as f32)
                                        .all(|d| d > MIN_JOINT_SPACING)
                                    {
                                        potential_joints.0.push(DisplayableJoint {
                                            entities: (
                                                contact_pair.collider1.entity(),
                                                contact_pair.collider2.entity(),
                                            ),
                                            points: (point.local_p1, point.local_p2),
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
}

fn update_predelete_joints(
    holdables: Query<&GlobalTransform, With<Holdable>>,
    mut predelete_joints: ResMut<PredeleteJoints>,
    players: Query<(&Holding, &Modifying, &Children)>,
    joint_handles: Query<(Entity, &JointHandleComponent)>,
    joint_set: ResMut<JointSet>,
    hold_points: QuerySet<(
        Query<(), With<HoldPoint>>,
        Query<&GlobalTransform, With<HoldPoint>>,
    )>,
    camera_orbit_centers: Query<&Children>,
) {
    predelete_joints.0.clear();

    if let Some((holding, modifying, player_children)) = players.iter().next() {
        if !holding.0 && modifying.0 {
            if let Some(entity) =
                get_hold_point_entity(player_children, camera_orbit_centers, hold_points.q0())
            {
                if let Ok(hold_point_position) = hold_points.q1().get(entity) {
                    for (joint_entity, joint_handle) in joint_handles.iter() {
                        if let Some(joint) = joint_set.get(joint_handle.handle()) {
                            if let Ok(transform) = holdables.get(joint.body1.entity()) {
                                if let Some(ball_joint) = joint.params.as_ball_joint() {
                                    let center = transform.translation
                                        + transform
                                            .rotation
                                            .mul_vec3(ball_joint.local_anchor1.into());
                                    if (center - hold_point_position.translation).length()
                                        < DELETE_RADIUS
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
    }
}

fn attach(
    mut commands: Commands,
    mut attach_events: EventReader<AttachEvent>,
    attach_points: Res<PotentialJoints>,
) {
    if attach_events.iter().next().is_some() {
        for DisplayableJoint { points, entities } in attach_points.0.iter() {
            commands.spawn().insert(JointBuilderComponent::new(
                BallJoint::new(points.0, points.1),
                entities.0,
                entities.1,
            ));
        }
    }
}

fn delete_joints(
    mut commands: Commands,
    predelete_joints: Res<PredeleteJoints>,
    mut clicks: EventReader<PlayerClick>,
) {
    if clicks.iter().next().is_some() {
        for PredeleteJoint { entity, .. } in predelete_joints.0.iter() {
            commands.entity(*entity).despawn_recursive();
        }
    }
}
