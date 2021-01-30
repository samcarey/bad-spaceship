use crate::plugins::{environment::map, player};
use crate::utils::{self, Vec3Ext};
use bevy::prelude::*;
use bevy_rapier3d::{physics::RapierConfiguration, rapier::geometry::ColliderBuilder};
use bevy_rapier3d::{physics::RigidBodyHandleComponent, rapier::dynamics::RigidBodyBuilder};
use player::{CameraOrbitCenter, FocusedInteractable, Holding};
use rand::Rng;
use rapier3d::{
    dynamics::{RigidBody, RigidBodySet},
    math::Vector,
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
            .add_resource(HoldingConfig::new(30.0, 30.0))
            .add_system(
                get_active_hold_point_and_part
                    .system()
                    .chain(hold_part.system()),
            )
            .add_system(orient_part.system());
    }
}

const NUM_PARTS: i32 = 10;
const PART_SIZE: f32 = 1.0;

struct Interactable;

pub struct Holdable;

struct GetsReplaced;

pub struct TargetOrientation(pub Quat);

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

fn get_active_hold_point_and_part(
    players: Query<(&Children, &FocusedInteractable, &Holding)>,
    camera_orbit_centers: Query<&Children, With<CameraOrbitCenter>>,
    hold_points: Query<&GlobalTransform, With<player::HoldPoint>>,
) -> Option<(Entity, Entity)> {
    if let Some((player_children, focused_interactable, holding)) = players.iter().next() {
        if let Some(current) = focused_interactable.current {
            if holding.0 {
                for player_child in player_children.iter() {
                    if let Ok(camera_orbit_center_children) =
                        camera_orbit_centers.get(*player_child)
                    {
                        for camera_orbit_center_child in camera_orbit_center_children.iter() {
                            if let Ok(_hold_point) = hold_points.get(*camera_orbit_center_child) {
                                return Some((camera_orbit_center_child.clone(), current.clone()));
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

struct HoldingConfig {
    positioning_stiffness: f32,
    positioning_damping: f32,
    orientation_stiffness: f32,
    orientation_damping: f32,
}

impl HoldingConfig {
    pub fn new(positioning_stiffness: f32, orientation_stiffness: f32) -> Self {
        Self {
            positioning_stiffness,
            // Critically damped
            positioning_damping: 2.0 * positioning_stiffness.sqrt(),
            orientation_stiffness,
            // Critically damped
            orientation_damping: 2.0 * orientation_stiffness.sqrt(),
        }
    }

    pub fn calc_positioning_force(
        &self,
        hold_point: &GlobalTransform,
        part_transform: &Transform,
        rb: &RigidBody,
    ) -> Vector<f32> {
        let vector_between: Vec3 = hold_point.translation - part_transform.translation;
        let positioning_acceleration = vector_between.to_vector() * self.positioning_stiffness
            - self.positioning_damping * rb.linvel();
        positioning_acceleration * rb.mass()
    }

    pub fn calc_orientating_torque(
        &self,
        hold_orientation: &Quat,
        part_transform: &Transform,
        rb: &mut RigidBody,
    ) -> Vector<f32> {
        let rotation_between: Quat = part_transform.rotation.conjugate() * hold_orientation.clone();
        let angular_acceleration = rotation_between.to_rotation_vector()
            * self.orientation_stiffness
            - self.orientation_damping * rb.angvel();
        let inertia_sqrt = rb.effective_world_inv_inertia_sqrt.inverse_unchecked();
        inertia_sqrt * (inertia_sqrt * angular_acceleration)
    }
}

fn hold_part(
    In(hold_point_and_part_entity): In<Option<(Entity, Entity)>>,
    hold_points: Query<&GlobalTransform, With<player::HoldPoint>>,
    parts: Query<(&Transform, &RigidBodyHandleComponent), With<Holdable>>,
    mut bodies: ResMut<RigidBodySet>,
    physics_config: Res<RapierConfiguration>,
    holding_config: Res<HoldingConfig>,
) {
    if let Some((hold_point_entity, part_entity)) = hold_point_and_part_entity {
        if let Ok(hold_point) = hold_points.get(hold_point_entity) {
            if let Ok((part_transform, part_rb_handle)) = parts.get(part_entity) {
                if let Some(rb) = bodies.get_mut(part_rb_handle.handle()) {
                    let gravity_cancelation_force = -rb.mass() * physics_config.gravity;
                    let positioning_force =
                        holding_config.calc_positioning_force(hold_point, part_transform, rb);
                    rb.apply_force(positioning_force + gravity_cancelation_force, true);
                }
            }
        }
    }
}

fn orient_part(
    parts: Query<(&Transform, &TargetOrientation, &RigidBodyHandleComponent)>,
    mut bodies: ResMut<RigidBodySet>,
    holding_config: Res<HoldingConfig>,
) {
    for (transform, target_orientation, part_rb_handle) in parts.iter() {
        if let Some(rb) = bodies.get_mut(part_rb_handle.handle()) {
            let torque =
                holding_config.calc_orientating_torque(&target_orientation.0, transform, rb);
            rb.apply_torque(torque, true);
        }
    }
}
