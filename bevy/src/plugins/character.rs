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
struct MoveSpeed(f32);
struct JumpForce(f32);

#[derive(ConfigFromFileMacro, Deserialize)]
struct Config {
    size: f32,
    name: String,
    max_speed: f32,
    jump_force: f32,
}

pub fn spawn(commands: &mut Commands) -> f32 {
    let config = Config::new(CONFIG_FILE);
    let rigid_body = RigidBodyBuilder::new_dynamic().translation(0.0, 10.0, 0.0);
    let collider = ColliderBuilder::cuboid(config.size / 2.0, config.size / 2.0, config.size / 2.0);

    commands
        .spawn((rigid_body, collider))
        .with(MoveSpeed(config.max_speed))
        .with(JumpForce(config.jump_force))
        .with(Name(config.name));
    return config.size;
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
    jump_force: &JumpForce,
) {
    if let Some(mut rb) = bodies.get_mut(rigid_body.handle()) {
        rb.wake_up();

        //
        // Get the current velocity from the physics engine
        //
        let current_velocity = rb.linvel.clone_owned();

        //
        // In moving the character we want to use two different physics principles: impulse and force.
        //
        // Since we want the character's movement in the horizontal plane (x,z) to be precisely controlled
        // WRT movement and stop via keypresses, we use rapier to apply an impulse for movement,
        // and then negate that impulse to stop instantaneously.  We need a different approach for
        // the vertical plane; if the same is applied to the vertical plane (y), the character will hover
        // instead of responding to gravity. In the vertical direction we want to apply "force" which then
        // releases and allows the rapier gravity to re-engage.
        //
        // To accomplish this, we compute separate vectors for horizontal/vertical contributions
        // and then use them to apply separate impulse/force actions (respectively) to our rigid body.
        //

        //
        // Start with the horizontal plane (x,z)
        // Compute our desired horizontal velocity vector and apply an impulse to the rigid body.
        //
        {
            //
            // Compute our desired horizontal velocity vector based on keyboard inputs and move speed
            //  Note: Horizontal plane = (x,z), Vertical plane = (y)
            //
            let forward = transform.value().z_axis().truncate() * keyboard_directional_input.0.z();
            let right = -transform.value().x_axis().truncate() * keyboard_directional_input.0.x();
            let desired_horizontal_velocity =
                vec3_to_vector(Vec3::from(forward + right)) * move_speed.0;

            //
            // get a copy of the current velocity from rapier, isolated to horizontal components only
            // (ie, zero out current vertical [y] component)
            //
            let current_horizontal_velocity = vec3_to_vector(Vec3::new(
                current_velocity[(0, 0)],
                0.0,
                current_velocity[(2, 0)],
            ));

            //
            // To move the character, we increase the speed to match the maximum speed in whatever
            // direction is indicated by user keypress; or, if no keys pressed then we cancel out
            // any velocity to stop horizontally.
            //
            let horizontal_velocity_change = match desired_horizontal_velocity.amax() > 0.0 {
                true => {
                    let current_speed_along_propulsion_direction = current_velocity
                        .dot(&desired_horizontal_velocity.normalize())
                        .abs();
                    let current_velocity_along_propulsion_direction =
                        match current_horizontal_velocity.amax() > 0.0 {
                            true => {
                                current_speed_along_propulsion_direction
                                    * current_horizontal_velocity.normalize()
                            }
                            false => Vector::zeros(),
                        };
                    desired_horizontal_velocity - current_velocity_along_propulsion_direction
                }
                false => -current_horizontal_velocity,
            };

            //
            // Apply the computed impulse to the character's rigid body
            //
            let horizontal_impulse = rb.mass() * horizontal_velocity_change;
            rb.apply_impulse(horizontal_impulse);
        }

        //
        // Now consider the vertical plane (y)
        // Compute our desired vertical velocity vector and apply a force to the rigid body.
        //
        {
            //
            // Compute our desired vertical velocity vector based on keyboard inputs and move speed
            //  Note: Horizontal plane = (x,z), Vertical plane = (y)
            //
            //  Note: We presume that keyboard directional input is limited externally.  If not,
            //          then a long keypress will act more like "thrust" upwards than singular
            //          jump event.
            //
            let up = transform.value().y_axis().truncate() * keyboard_directional_input.0.y();
            let desired_vertical_velocity = vec3_to_vector(Vec3::from(up)) * jump_force.0;

            //
            // get a copy of the current velocity from rapier, isolated to vertical component only
            // (ie, zero out current horizontal [x,z] components)
            //
            let current_vertical_velocity =
                vec3_to_vector(Vec3::new(0.0, current_velocity[(1, 0)], 0.0));

            //
            // To "jump" we allow apply force in the vertical direction
            //
            let vertical_velocity = match desired_vertical_velocity.amax() > 0.0 {
                true => {
                    let current_speed_along_propulsion_direction = current_velocity
                        .dot(&desired_vertical_velocity.normalize())
                        .abs();
                    let current_velocity_along_propulsion_direction =
                        match current_vertical_velocity.amax() > 0.0 {
                            true => {
                                current_speed_along_propulsion_direction
                                    * current_vertical_velocity.normalize()
                            }
                            false => Vector::zeros(),
                        };
                    desired_vertical_velocity - current_velocity_along_propulsion_direction
                }
                false => Vector::zeros(),
            };

            //
            // Apply the computed force to the character's rigid body
            //
            let vertical_force = rb.mass() * vertical_velocity;
            rb.apply_force(vertical_force);
        }
    }
}
