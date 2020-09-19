use bevy::prelude::*;
use bevy_rapier3d::physics::RigidBodyHandleComponent;
use bevy_rapier3d::rapier::dynamics::RigidBodyBuilder;
use bevy_rapier3d::rapier::geometry::ColliderBuilder;
use config_from_file_macro::ConfigFromFileMacro;
use config_from_file_macro_derive::ConfigFromFileMacro;
use rapier3d::dynamics::RigidBodySet;
use rapier3d::math::Vector;
use serde::Deserialize;

use crate::plugins::player;
pub struct CharacterPlugin;

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_system(move_character_based_on_keyboard_input.system());
    }
}

const CONFIG_FILE: &str = "assets/config/character.ron";

struct Name(String);

pub struct MoveSpeed(f32);

#[derive(ConfigFromFileMacro, Deserialize)]
struct Config {
    size: f32,
    name: String,
    max_speed: f32,
}

pub fn spawn(commands: &mut Commands) {
    let config = Config::new(CONFIG_FILE);
    let rigid_body = RigidBodyBuilder::new_dynamic().translation(0.0, 50.0, 0.0);
    let collider = ColliderBuilder::cuboid(config.size / 2.0, config.size / 2.0, config.size / 2.0);

    commands
        .spawn((rigid_body, collider))
        .with(MoveSpeed(config.max_speed))
        .with(Name(config.name));
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
        // Note: Y is vertical in Bevy/Rapier, X/Z is horizontal
        let forward = transform.value.z_axis().truncate() * keyboard_directional_input.0.y();
        let right = -transform.value.x_axis().truncate() * keyboard_directional_input.0.x();
        let desired_velocity = vec3_to_vector(Vec3::from(forward + right)) * move_speed.0;

        // Get the current velocity from the physics engine but ignore the vertical component (Y)
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
