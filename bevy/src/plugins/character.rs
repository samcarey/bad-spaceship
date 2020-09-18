use bevy::prelude::*;
use bevy_rapier3d::physics::RigidBodyHandleComponent;
use bevy_rapier3d::rapier::dynamics::RigidBodyBuilder;
use bevy_rapier3d::rapier::geometry::ColliderBuilder;
use config_from_file_macro::ConfigFromFileMacro;
use config_from_file_macro_derive::ConfigFromFileMacro;
use rapier3d::dynamics::RigidBodySet;
use rapier3d::math;
use rapier3d::math::{Isometry, Matrix, Vector};
use serde::Deserialize;

use crate::plugins::player;
pub struct CharacterPlugin;

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_system(move_character_based_on_keyboard_input.system());
    }
}

const CONFIG_FILE: &str = "assets/config/character.ron";

const RADIANS_IN_CIRCLE: f32 = 2.0 * std::f32::consts::PI;

struct Name(String);
struct BasePosition(pub Vec3);
pub struct MoveSpeed(f32);
struct Bob {
    amplitude: f32,
    phase: f32,
    radians_per_second: f32,
}

#[derive(ConfigFromFileMacro, Deserialize)]
struct Config {
    size: f32,
    hover_size_ratio: f32,
    bob_ratio: f32,
    bob_rate: f32,
    extra_first_person_height_size_ratio: f32,
    name: String,
    max_speed: f32,
}

pub fn spawn(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) {
    let config = Config::new(CONFIG_FILE);

    let hover = config.size * config.hover_size_ratio;
    let base_height = config.size / 2.0 + hover;
    let static_height_of_top = config.size + hover;
    let bob_amplitude = hover * config.bob_ratio;
    let dynamic_height_of_top = static_height_of_top + bob_amplitude;
    let extra_first_person_height = config.extra_first_person_height_size_ratio * config.size;
    let first_person_camera_height = dynamic_height_of_top * extra_first_person_height;

    let base_position = BasePosition(Vec3::new(
        0.0,
        base_height * 55. - first_person_camera_height,
        0.0,
    ));

    // let cube_mat_handle = materials.add({
    //     let mut cube_material: StandardMaterial = Color::rgb(1.0, 1.0, 1.0).into();
    //     cube_material.shaded = true;
    //     cube_material
    // });

    let rigid_body = RigidBodyBuilder::new_dynamic().translation(0.0, 50.0, 0.0);
    let collider = ColliderBuilder::cuboid(config.size / 2.0, config.size / 2.0, config.size / 2.0);

    let character_entity = commands
        // .spawn(PbrComponents {
        //     mesh: meshes.add(Mesh::from(shape::Cube {
        //         size: config.size / 2.0,
        //     })),
        //     material: cube_mat_handle,
        //     translation: Translation(base_position.0),
        //     ..Default::default()
        // })
        .spawn((rigid_body, collider))
        .with(base_position)
        .with(MoveSpeed(config.max_speed))
        .with(Name(config.name))
        .with(Bob {
            amplitude: bob_amplitude,
            phase: 0.0,
            radians_per_second: config.bob_rate * RADIANS_IN_CIRCLE,
        });
    // .current_entity();
    // (character_entity, first_person_camera_height)
}

fn vec3_to_vector(v: Vec3) -> Vector<f32> {
    Vector::new(v.x(), v.y(), v.z())
}

fn move_character_based_on_keyboard_input(
    mut bodies: ResMut<RigidBodySet>,
    keyboard_directional_input: &player::KeyboardDirectionalInput,
    rigid_body: &RigidBodyHandleComponent,
    transform: &Transform,
    move_speed: &MoveSpeed,
) {
    if let Some(mut rb) = bodies.get_mut(rigid_body.handle()) {
        rb.wake_up();

        // Need to map the y-coordinate (used for forward in 2D vector that takes keyboard directional input)
        // to the z-coordinate (used by the 3D game engine for horizontal forward).
        // Note: Z is vertical in Bevy/Rapier, X/Y is horizontal
        let forward = transform.value.z_axis().truncate() * keyboard_directional_input.0.y();
        let right = -transform.value.x_axis().truncate() * keyboard_directional_input.0.x();
        let desired_velocity = vec3_to_vector(Vec3::from(forward + right)) * move_speed.0;

        // Get the current velocity from the physics engine but ignore the vertical component
        let current_velocity = rb.linvel.clone_owned();
        let current_horizontal_velocity = vec3_to_vector(Vec3::new(
            current_velocity[(0, 0)],
            0.0,
            current_velocity[(2, 0)],
        ));

        // Either increase the speed to match the maximum speed,
        // or cancel out any velocity to come to a halt.
        let velocity_change = match desired_velocity.amax() > 0.0 {
            true => {
                let current_speed_along_propulsion_direction =
                    current_horizontal_velocity.dot(&desired_velocity.normalize());
                let current_velocity_along_propulsion_direction =
                    match current_horizontal_velocity.amax() > 0.0 {
                        true => {
                            current_speed_along_propulsion_direction
                                * current_horizontal_velocity.normalize()
                        }
                        false => Vector::zeros(),
                    };
                desired_velocity - current_velocity_along_propulsion_direction
            }
            false => -current_horizontal_velocity,
        };

        let impulse = rb.mass() * velocity_change;
        rb.apply_impulse(impulse);
    }
}
