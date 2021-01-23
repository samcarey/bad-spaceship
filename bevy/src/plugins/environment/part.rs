use crate::plugins::{environment::map, player};
use crate::utils;
use bevy::prelude::*;
use bevy_rapier3d::rapier::dynamics::RigidBodyBuilder;
use bevy_rapier3d::rapier::geometry::ColliderBuilder;
use rand::Rng;
use std::f32;
pub struct PartPlugin;
// use crate::utils;

impl Plugin for PartPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_startup_system(spawn_parts.system())
            .add_system(replace_fallen_parts.system())
            .add_system(update_focused_interactables.system())
            .add_system(highlight_interactables.system());
    }
}

const NUM_PARTS: i32 = 10;
const PART_SIZE: f32 = 1.0;

#[derive(Default)]
struct Interactable;

struct GetsReplaced;

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
        Interactable::default(),
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
    mut players: Query<(&Transform, &mut player::FocusedInteractable), With<player::Player>>,
    mut interactables: Query<(&mut Transform, Entity), With<Interactable>>,
) {
    // Determine which iteractable entity each player is focused on (i.e. looking at, within range)

    for (player_transform, mut focused_interactable) in players.iter_mut() {
        // Search for the most appropriate interactable that should be focused by the player
        let mut smallest_angle = MAX_INTERACT_ANGLE;
        let mut new_focused_interactable = None;
        for (interactable_transform, interactable) in interactables.iter_mut() {
            let vector_between = interactable_transform.translation - player_transform.translation;
            if vector_between.length_squared() < MAX_INTERACT_DISTANCE_SQUARED {
                let angle_from_look = player_transform.forward().angle_between(vector_between);

                if angle_from_look < smallest_angle {
                    smallest_angle = angle_from_look;
                    new_focused_interactable = Some(interactable.clone());
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
