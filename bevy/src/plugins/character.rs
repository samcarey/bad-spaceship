use crate::{
    utils::{QuatExt, TransformExt, Vec3Ext},
    AppState, APP_STATE,
};
use bevy::prelude::*;
use bevy_rapier3d::physics::{ColliderHandleComponent, EventQueue, RigidBodyHandleComponent};
use bevy_rapier3d::rapier::dynamics::RigidBodyBuilder;
use bevy_rapier3d::rapier::geometry::ColliderBuilder;
use player::Player;
use rapier3d::{dynamics::RigidBodySet, geometry::ContactEvent};
use rapier3d::{
    geometry::ColliderHandle,
    math::{Isometry, Vector},
};
use serde::Deserialize;

use crate::plugins::player;

pub struct CharacterPlugin;

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.on_state_update(
            APP_STATE,
            AppState::InGame,
            move_character_based_on_keyboard_input.system(),
        )
        .add_system_to_stage(
            stage::POST_UPDATE,
            rotate_character_based_on_mouse_input.system(),
        )
        .add_system(touching_ground.system());
    }
}

struct Name(String);
struct MoveSpeed(f32);
struct JumpForce(f32);

#[derive(Default)]
struct Touching(Vec<ColliderHandle>);

impl Touching {
    pub fn index(&self, handle: &ColliderHandle) -> Option<usize> {
        self.0.iter().position(|x| *x == *handle)
    }

    pub fn touching(&self) -> bool {
        !self.0.is_empty()
    }
}

#[derive(Deserialize)]
struct Config {
    size: f32,
    name: String,
    max_speed: f32,
    jump_force: f32,
}

pub fn spawn(commands: &mut Commands) -> f32 {
    let config: Config = config_from_file!("character.ron");

    commands.spawn((
        RigidBodyBuilder::new_dynamic()
            .translation(0.0, 10.0, 0.0)
            .restrict_rotations(false, false, false),
        ColliderBuilder::ball(config.size / 2.0),
        MoveSpeed(config.max_speed),
        JumpForce(config.jump_force),
        Name(config.name),
        Touching::default(),
    ));
    return config.size;
}

fn touching_ground(
    mut players: Query<(&mut Touching, &ColliderHandleComponent), With<Player>>,
    events: Res<EventQueue>,
) {
    // TODO: Simplify this block?
    while let Ok(contact_event) = events.contact_events.pop() {
        for (mut touching, player_rb_handle) in players.iter_mut() {
            match contact_event {
                ContactEvent::Stopped(handle1, handle2) => {
                    if player_rb_handle.handle() == handle1 {
                        if let Some(index) = touching.index(&handle2) {
                            touching.0.remove(index);
                        }
                    } else if player_rb_handle.handle() == handle2 {
                        if let Some(index) = touching.index(&handle1) {
                            touching.0.remove(index);
                        }
                    }
                }
                ContactEvent::Started(handle1, handle2) => {
                    if player_rb_handle.handle() == handle1 {
                        if let None = touching.index(&handle2) {
                            touching.0.push(handle2);
                        }
                    } else if player_rb_handle.handle() == handle2 {
                        if let None = touching.index(&handle1) {
                            touching.0.push(handle1);
                        }
                    }
                }
            }

            // println!("Received contact event: {:?}", contact_event);
        }
    }
}

fn move_character_based_on_keyboard_input(
    mut bodies: ResMut<RigidBodySet>,
    query: Query<(
        &player::KeyboardDirectionalInput,
        &player::GameStickDirectionalInput,
        &RigidBodyHandleComponent,
        &Transform,
        &MoveSpeed,
        &JumpForce,
        &Touching,
    )>,
) {
    for (
        keyboard_directional_input,
        gamepad_directional_input,
        rigid_body,
        transform,
        move_speed,
        jump_force,
        touch_tracker,
    ) in query.iter()
    {
        if let Some(rb) = bodies.get_mut(rigid_body.handle()) {
            //
            // Get the current velocity from the physics engine
            //
            let current_velocity = rb.linvel().clone_owned();

            //
            // Combine the keyboard and gamepad directional inputs
            //
            let mut combined_directional_input = Vec3::zero();
            combined_directional_input.x =
                keyboard_directional_input.0.x + gamepad_directional_input.0.x;
            combined_directional_input.y =
                keyboard_directional_input.0.y + gamepad_directional_input.0.y;
            combined_directional_input.z =
                keyboard_directional_input.0.z + gamepad_directional_input.0.z;
            if combined_directional_input != Vec3::zero() {
                combined_directional_input.normalize();
            }

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
                let forward = transform.forward() * combined_directional_input.z;
                let right = -transform.right() * combined_directional_input.x;
                let desired_horizontal_velocity =
                    Vec3::from(forward + right).to_vector() * move_speed.0;

                //
                // get a copy of the current velocity from rapier, isolated to horizontal components only
                // (ie, zero out current vertical [y] component)
                //
                let current_horizontal_velocity =
                    Vec3::new(current_velocity[(0, 0)], 0.0, current_velocity[(2, 0)]).to_vector();

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

                // Apply the computed impulse to the character's rigid body
                let mut horizontal_impulse = rb.mass() * horizontal_velocity_change * 0.13; // slowing down with fudge factor

                if !touch_tracker.touching() {
                    horizontal_impulse *= 0.13; // slowing down even more when in air
                }

                rb.apply_impulse(horizontal_impulse, true);
            }

            // TODO: Update this documentation and variable names,
            // since we're doing an impulse instead of force now.
            //
            // Now consider the vertical plane (y)
            // Compute our desired vertical velocity vector and apply a force to the rigid body.
            //
            if touch_tracker.touching() {
                //
                // Compute our desired vertical velocity vector based on keyboard inputs and move speed
                //  Note: Horizontal plane = (x,z), Vertical plane = (y)
                //
                //  Note: We presume that keyboard directional input is limited externally.  If not,
                //          then a long keypress will act more like "thrust" upwards than singular
                //          jump event.
                //
                let up = transform.up() * combined_directional_input.y;
                let desired_vertical_velocity = Vec3::from(up).to_vector() * jump_force.0;

                //
                // get a copy of the current velocity from rapier, isolated to vertical component only
                // (ie, zero out current horizontal [x,z] components)
                //
                let current_vertical_velocity =
                    Vec3::new(0.0, current_velocity[(1, 0)], 0.0).to_vector();

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
                rb.apply_impulse(vertical_force, true);
            }
        }
    }
}

fn rotate_character_based_on_mouse_input(
    mut bodies: ResMut<RigidBodySet>,
    query: Query<(&RigidBodyHandleComponent, &Transform, &player::Yaw), With<player::Player>>,
) {
    for (rigid_body, transform, yaw) in query.iter() {
        if let Some(rb) = bodies.get_mut(rigid_body.handle()) {
            let rotation = Quat::from_rotation_y(-yaw.0).to_unit_quaternion();
            let position = Isometry::from_parts(transform.translation.to_translation3(), rotation);
            rb.set_position(position, true);
        }
    }
}
