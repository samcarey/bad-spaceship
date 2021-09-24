use crate::map::PLATFORM_WIDTH_M;
use crate::utils::{self, Orderable, QuatExt, TransformExt};
use crate::{
    Attachable, CameraOrbitCenter, Focused, FocusedInteractable, HoldPoint, Holding, Player,
};
use bevy::prelude::*;
use bevy_rapier3d::na::Vector3;
use bevy_rapier3d::physics::{
    ColliderBundle, ColliderPositionSync, IntoEntity, RapierConfiguration, RigidBodyBundle,
};
use bevy_rapier3d::prelude::{
    ActiveEvents, ColliderMassProps, ColliderMaterial, ColliderShape, ContactEvent,
    RigidBodyForces, RigidBodyMassProps, RigidBodyVelocity, SdpMatrix,
};
use bevy_rapier3d::render::ColliderDebugRender;
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
            .add_system(add_attachable.system())
            .add_system(remove_attachable.system());
    }
}

const NUM_PARTS: i32 = 10;
const PART_SIZE: f32 = 1.0;
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
    inertia_sqrt: SdpMatrix<f32>,
    oscillator: CriticallyDampedHarmonicOscillator,
}

impl TargetOrientation {
    pub fn new(mass_properties: &RigidBodyMassProps, quat: Quat) -> Self {
        Self {
            quat,
            inertia_sqrt: mass_properties.effective_world_inv_inertia_sqrt,
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
                shape: ColliderShape::cuboid(PART_SIZE / 2.0, PART_SIZE / 2.0, PART_SIZE / 2.0),
                mass_properties: ColliderMassProps::Density(2.0),
                material: ColliderMaterial {
                    friction: 1.0,
                    restitution: 0.1,
                    ..Default::default()
                },
                flags: (ActiveEvents::INTERSECTION_EVENTS | ActiveEvents::CONTACT_EVENTS).into(),
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

fn add_attachable(
    mut commands: Commands,
    helds: Query<Entity, With<TargetPosition>>,
    potential_attachables: Query<Entity, (With<Holdable>, Without<Attachable>)>,
    mut contact_events: EventReader<ContactEvent>,
) {
    for contact_event in contact_events
        .iter()
        .filter_map(|x| x.order(&(|entity| helds.get(entity).is_ok())))
    {
        if let ContactEvent::Started(_handle1, handle2) = contact_event {
            if let Ok(entity) = potential_attachables.get(handle2.entity()) {
                commands.entity(entity).insert(Attachable);
            }
        }
    }
}

fn remove_attachable(
    mut commands: Commands,
    helds: Query<Entity, With<TargetPosition>>,
    potential_attachables: Query<Entity, (With<Holdable>, With<Attachable>)>,
    mut contact_events: EventReader<ContactEvent>,
) {
    for contact_event in contact_events
        .iter()
        .filter_map(|x| x.order(&(|entity| helds.get(entity).is_ok())))
    {
        if let ContactEvent::Stopped(_handle1, handle2) = contact_event {
            if let Ok(entity) = potential_attachables.get(handle2.entity()) {
                commands.entity(entity).remove::<Attachable>();
            }
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
    )>,
) {
    for (part_transform, target_orientation, velocity, mut forces) in parts.iter_mut() {
        let rotation_between =
            (target_orientation.quat * part_transform.rotation.conjugate()).to_rotation_vector();
        let angular_acceleration = target_orientation
            .oscillator
            .calculate_acceleration(&rotation_between, &velocity.angvel);
        let torque = target_orientation.inertia_sqrt
            * (target_orientation.inertia_sqrt * angular_acceleration);
        forces.torque += torque;
    }
}
