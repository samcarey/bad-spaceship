use crate::plugins::{environment::map, player};
use crate::utils::{self, Vec3Ext};
use bevy::prelude::*;
use bevy_rapier3d::{physics::RapierConfiguration, rapier::geometry::ColliderBuilder};
use bevy_rapier3d::{physics::RigidBodyHandleComponent, rapier::dynamics::RigidBodyBuilder};
use player::CameraOrbitCenter;
use rand::Rng;
use rapier3d::{
    dynamics::{RigidBody, RigidBodySet},
    math::{SdpMatrix, Vector},
};
use std::f32;
use utils::QuatExt;

pub struct PartPlugin;

impl Plugin for PartPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_startup_system(spawn_parts.system())
            .add_system(replace_fallen_parts.system())
            .add_system(update_focused_interactables.system())
            .add_system(highlight_interactables.system())
            .add_system(
                // get_active_hold_point_and_part
                // .system()
                // .chain(
                position_held_part.system(), // ),
            )
            .add_system(orient_held_part.system());
    }
}

const NUM_PARTS: i32 = 10;
const PART_SIZE: f32 = 1.0;
const POSITIONING_STIFFNESS: f32 = 30.0;
const ORIENTING_STIFFNESS: f32 = 5.0;

struct Interactable;

pub struct Holdable;

struct GetsReplaced;

struct CrirticallyDampedHarmonicOscillator {
    stiffness: f32,
    damping: f32,
}

impl CrirticallyDampedHarmonicOscillator {
    pub fn new(stiffness: f32) -> Self {
        Self {
            stiffness,
            damping: 2.0 * stiffness.sqrt(),
        }
    }

    pub fn calculate_acceleration(
        &self,
        displacement: &Vector<f32>,
        velocity: &Vector<f32>,
    ) -> Vector<f32> {
        displacement * self.stiffness - velocity * self.damping
    }
}

pub struct TargetPosition {
    hold_point_entity: Entity,
    oscillator: CrirticallyDampedHarmonicOscillator,
}

impl TargetPosition {
    pub fn new(hold_point_entity: Entity) -> Self {
        Self {
            hold_point_entity,
            oscillator: CrirticallyDampedHarmonicOscillator::new(POSITIONING_STIFFNESS),
        }
    }
}

pub struct TargetOrientation {
    pub quat: Quat,
    inertia_sqrt: SdpMatrix<f32>,
    oscillator: CrirticallyDampedHarmonicOscillator,
}

impl TargetOrientation {
    pub fn new(rb: &RigidBody, quat: Quat) -> Self {
        Self {
            quat,
            inertia_sqrt: rb.effective_world_inv_inertia_sqrt.inverse_unchecked(),
            oscillator: CrirticallyDampedHarmonicOscillator::new(ORIENTING_STIFFNESS),
        }
    }
}

const SPAWN_ZONE_HALF_WIDTH: f32 = map::PLATFORM_WIDTH_M / 2.0 * 0.7;

fn spawn_part(commands: &mut Commands) {
    let mut rng = rand::thread_rng();
    commands.spawn((
        RigidBodyBuilder::new_dynamic().translation(
            rng.gen_range(-SPAWN_ZONE_HALF_WIDTH..=SPAWN_ZONE_HALF_WIDTH),
            rng.gen_range(5.0..=15.0),
            rng.gen_range(-SPAWN_ZONE_HALF_WIDTH..=SPAWN_ZONE_HALF_WIDTH),
        ),
        ColliderBuilder::cuboid(PART_SIZE / 2.0, PART_SIZE / 2.0, PART_SIZE / 2.0)
            .friction(1.0)
            .density(2.0),
        Interactable,
        Holdable,
        GetsReplaced,
    ));
}

fn spawn_parts(commands: &mut Commands) {
    for _ in 0..NUM_PARTS {
        spawn_part(commands);
    }
}

fn replace_fallen_parts(
    commands: &mut Commands,
    parts: Query<(&Transform, Entity), With<GetsReplaced>>,
) {
    for (transform, entity) in parts.iter() {
        if transform.translation.y < -10.0 {
            commands.despawn(entity);
            spawn_part(commands);
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
                material.albedo = focused_interactable.previous_color.unwrap();
            }
        }

        // Now, store the newly focused interactable's color and then higlight it
        if let Some(current) = focused_interactable.current {
            if let Ok(material_handle) = interactables.get_mut(current) {
                let color = &mut materials.get_mut(&*material_handle).unwrap().albedo;
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
    parts: Query<(&Transform, &RigidBodyHandleComponent, &TargetPosition)>,
    mut bodies: ResMut<RigidBodySet>,
    physics_config: Res<RapierConfiguration>,
) {
    for (part_transform, part_rb_handle, target_position) in parts.iter() {
        if let Ok(hold_point_position) = hold_points.get(target_position.hold_point_entity) {
            if let Some(rb) = bodies.get_mut(part_rb_handle.handle()) {
                let vector_between =
                    (hold_point_position.translation - part_transform.translation).to_vector();
                let positioning_acceleration = target_position
                    .oscillator
                    .calculate_acceleration(&vector_between, rb.linvel());

                let gravity_cancelation_force = -rb.mass() * physics_config.gravity;
                let positioning_force = positioning_acceleration * rb.mass();
                rb.apply_force(positioning_force + gravity_cancelation_force, true);
            }
        }
    }
}

fn orient_held_part(
    parts: Query<(&Transform, &TargetOrientation, &RigidBodyHandleComponent)>,
    mut bodies: ResMut<RigidBodySet>,
) {
    for (part_transform, target_orientation, part_rb_handle) in parts.iter() {
        if let Some(rb) = bodies.get_mut(part_rb_handle.handle()) {
            let rotation_between = (target_orientation.quat * part_transform.rotation.conjugate())
                .to_rotation_vector();
            let angular_acceleration = target_orientation
                .oscillator
                .calculate_acceleration(&rotation_between, rb.angvel());
            let torque = target_orientation.inertia_sqrt
                * (target_orientation.inertia_sqrt * angular_acceleration);
            rb.apply_torque(torque, true);
        }
    }
}
