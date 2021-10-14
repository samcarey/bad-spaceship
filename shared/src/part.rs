use crate::map::PLATFORM_WIDTH_M;
use crate::utils::{self, QuatExt, TransformExt};
use crate::{
    Attachable, CameraOrbitCenter, Focused, FocusedInteractable, HoldPoint, Holding, Player,
    ReleaseEvent,
};
use bevy::prelude::*;
use bevy_rapier3d::na::Vector3;
use bevy_rapier3d::physics::{
    ColliderBundle, ColliderPositionSync, IntoEntity, IntoHandle, JointBuilderComponent,
    RapierConfiguration, RigidBodyBundle,
};
use bevy_rapier3d::prelude::{
    ActiveEvents, BallJoint, ColliderFlags, ColliderMassProps, ColliderMaterial, ColliderShape,
    NarrowPhase, RigidBodyForces, RigidBodyMassProps, RigidBodyVelocity,
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
            .add_system(attach.system());
    }
}

const NUM_PARTS: i32 = 10;
pub const MAX_PART_SIZE: f32 = 10.0;
const MIN_PART_SIZE: f32 = 0.1;
const MIN_PART_VOLUME: f32 = 1.0;
const MAX_PART_VOLUME: f32 = 2.0;
const POSITIONING_STIFFNESS: f32 = 30.0;
const ORIENTING_STIFFNESS: f32 = 5.0;

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
    hold_point_entity: Entity,
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
        commands
            .spawn()
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
                shape: get_random_shape(&mut rng),
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
    mut players: Query<(&mut FocusedInteractable, &Holding, &Children), With<Player>>,
    mut interactables: Query<(&mut Transform, Entity), With<Interactable>>,
    camera_orbit_centers: Query<&GlobalTransform, With<CameraOrbitCenter>>,
) {
    // Determine which iteractable entity each player is focused on (i.e. looking at, within range)
    for (mut focused_interactable, holding, player_children) in players.iter_mut() {
        if !holding.0 {
            let mut newly_focused_interactable_option = None;
            for player_child in player_children.iter() {
                if let Ok(camera_orbit_center) = camera_orbit_centers.get(*player_child) {
                    // Search for the most appropriate interactable that should be focused by the player
                    let mut smallest_angle = MAX_INTERACT_ANGLE;

                    for (interactable_transform, interactable) in interactables.iter_mut() {
                        let vector_between =
                            interactable_transform.translation - camera_orbit_center.translation;
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

fn attach(
    mut commands: Commands,
    mut attach_events: EventReader<ReleaseEvent>,
    holdables: Query<(), With<Holdable>>,
    narrow_phase: Res<NarrowPhase>,
) {
    for attach_event in attach_events.iter() {
        for contact_pair in narrow_phase
            .contacts_with(attach_event.primary_entity.handle())
            .filter(|x| x.has_any_active_contact)
        {
            let ordered = contact_pair.collider1.entity() == attach_event.primary_entity;
            let other_entity = if ordered {
                contact_pair.collider2.entity()
            } else {
                contact_pair.collider1.entity()
            };
            if holdables.get(other_entity).is_ok() {
                for manifold in &contact_pair.manifolds {
                    for point in &manifold.points {
                        // This can be used to prevent attached colliders from interacting, but I don't think it's necessary right now.
                        // I originally added this because I thought the physics would be unstable otherwise, but it's actually OK.
                        //
                        // commands
                        //     .entity(attach_event.primary_entity)
                        //     .insert(IgnoreContactsWith(other_entity));
                        // commands
                        //     .entity(other_entity)
                        //     .insert(IgnoreContactsWith(attach_event.primary_entity));

                        let points = if ordered {
                            (point.local_p1, point.local_p2)
                        } else {
                            (point.local_p2, point.local_p1)
                        };

                        commands.spawn().insert(JointBuilderComponent::new(
                            BallJoint::new(points.0, points.1),
                            attach_event.primary_entity,
                            other_entity,
                        ));
                    }
                }
            }
        }
    }
}
