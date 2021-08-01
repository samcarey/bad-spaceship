use crate::plugins::{environment::map, player};
use crate::utils::{self};
use bevy::prelude::*;
use bevy_rapier3d::na::Vector3;
use bevy_rapier3d::physics::{
    ColliderBundle, ColliderPositionSync, RapierConfiguration, RigidBodyBundle,
};
use bevy_rapier3d::prelude::{
    ColliderMassProps, ColliderMaterial, ColliderShape, RigidBodyForces, RigidBodyMassProps,
    RigidBodyVelocity, SdpMatrix,
};
use bevy_rapier3d::render::ColliderDebugRender;
use player::CameraOrbitCenter;
use rand::Rng;
use std::f32;
use utils::{QuatExt, TransformExt};

pub struct PartPlugin;

impl Plugin for PartPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_startup_system(spawn_initial_parts.system())
            .add_system(replace_fallen_parts.system())
            .add_system(update_focused_interactables.system())
            .add_system(highlight_interactables.system())
            .add_system(position_held_part.system())
            .add_event::<NewPart>()
            .add_system(orient_held_part.system())
            .add_system(spawn_part.system());
    }
}

const NUM_PARTS: i32 = 10;
const PART_SIZE: f32 = 1.0;
const POSITIONING_STIFFNESS: f32 = 30.0;
const ORIENTING_STIFFNESS: f32 = 5.0;

struct Interactable;

pub struct Holdable;

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

const SPAWN_ZONE_HALF_WIDTH: f32 = map::PLATFORM_WIDTH_M / 2.0 * 0.7;

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
                ..Default::default()
            })
            .insert_bundle((
                Interactable,
                Holdable,
                GetsReplaced,
                ColliderDebugRender::with_id(1),
                ColliderPositionSync::Discrete,
            ));
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

fn update_focused_interactables(
    mut players: Query<
        (
            &mut player::FocusedInteractable,
            &player::Holding,
            &Children,
        ),
        With<player::Player>,
    >,
    mut interactables: Query<(&mut Transform, Entity), With<Interactable>>,
    camera_orbit_centers: Query<&GlobalTransform, With<CameraOrbitCenter>>,
) {
    // Determine which iteractable entity each player is focused on (i.e. looking at, within range)

    for (mut focused_interactable, holding, player_children) in players.iter_mut() {
        if holding.0 {
            return;
        }

        let mut new_focused_interactable = None;
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
                            new_focused_interactable = Some(interactable.clone());
                        }
                    }
                }
            }
        }

        // If a new interactable should be focused by this player, first save the previous one
        if let Some(newly_focused) = new_focused_interactable {
            if let Some(currently_focused) = focused_interactable.current {
                // There was something before, and something now
                if newly_focused != currently_focused {
                    // The new thing is different from the previous thing
                    focused_interactable.previous = Some(currently_focused);
                    focused_interactable.current = Some(newly_focused);
                }
            } else {
                // There wasn't anything before
                focused_interactable.current = Some(newly_focused)
            }
        } else {
            // There should not be anything
            if let Some(currently_focused) = focused_interactable.current {
                // There was something before
                focused_interactable.previous = Some(currently_focused);
                focused_interactable.current = None;
            }
        }
    }
}

fn highlight_interactables(
    mut interactors: Query<&mut player::FocusedInteractable, Changed<player::FocusedInteractable>>,
    mut interactables: Query<&Handle<StandardMaterial>, With<Interactable>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for mut focused_interactable in interactors.iter_mut() {
        // First, restore the previously focused interactable to its original color
        if let Some(previous) = focused_interactable.previous {
            if let Ok(material_handle) = interactables.get_mut(previous) {
                let material = materials.get_mut(&*material_handle).unwrap();
                material.base_color = focused_interactable.previous_color.unwrap();
            }
        }

        // Now, store the newly focused interactable's color and then higlight it
        if let Some(current) = focused_interactable.current {
            if let Ok(material_handle) = interactables.get_mut(current) {
                let color = &mut materials.get_mut(&*material_handle).unwrap().base_color;
                focused_interactable.previous_color = Some(color.clone());

                // Make more yellowish
                color.set_g((color.g() + 0.75).min(1.0));
                color.set_r((color.r() + 0.75).min(1.0));
            }
        }
    }
}

fn position_held_part(
    hold_points: Query<&GlobalTransform, With<player::HoldPoint>>,
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
