use crate::plugins::environment::map;
// use crate::plugins::player;
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
            // .add_system(grabability.system())
            ;
    }
}

const NUM_PARTS: i32 = 3;
const PART_SIZE: f32 = 1.0;

struct Grabable();

fn spawn_parts(commands: &mut Commands) {
    let mut rng = rand::thread_rng();
    let spawn_zone_half_width = map::PLATFORM_WIDTH_M / 2.0 * 0.7;
    for _ in 0..NUM_PARTS {
        commands.spawn((
            RigidBodyBuilder::new_dynamic().translation(
                rng.gen_range(-spawn_zone_half_width..=spawn_zone_half_width),
                rng.gen_range(5.0..=15.0),
                rng.gen_range(-spawn_zone_half_width..=spawn_zone_half_width),
            ),
            ColliderBuilder::cuboid(PART_SIZE / 2.0, PART_SIZE / 2.0, PART_SIZE / 2.0)
                .friction(1.0)
                .density(2.0),
            Grabable(),
        ));
    }
}

// const MAX_GRAB_DISTANCE: f32 = 3.0;
// const MAX_GRAB_ANGLE_DEGREES: f32 = 20.0;
// const MAX_GRAB_ANGLE: f32 = MAX_GRAB_ANGLE_DEGREES * utils::DEG_TO_RADIANS;

// fn grabability(
//     mut player_query: Query<(&mut player::Player, &Transform)>,
//     mut part_query: Query<(&mut Grabable, &mut Transform, &mut StandardMaterial)>,
// ) {
//     for (player_component, player_transform) in player_query.iter_mut() {
//         let mut smallest_angle = MAX_GRAB_ANGLE;
//         let mut best_part: Option<Entity> = None;
//         for (_grabable, part_transform, mut part_material) in &mut part_query.iter() {
//             let vector_between = utils::vec3_to_vector(
//                 part_transform.translation() - player_transform.translation(),
//             );
//             part_material.albedo = Color::rgb(0.0, 0.0, 1.0);
//             if vector_between.norm() < MAX_GRAB_DISTANCE {
//                 let angle_from_look = vector_between.angle(&vector_between);
//                 if angle_from_look < smallest_angle {
//                     smallest_angle = angle_from_look;
//                     part_material.albedo = Color::rgb(1.0, 1.0, 0.0);
//                 }
//             }
//         }
//     }
// }
