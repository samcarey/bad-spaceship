use bevy::prelude::*;
use bevy_rapier3d::{
    na::{Isometry, UnitQuaternion, Vector3},
    physics::{ColliderBundle, ColliderPositionSync, IntoEntity, RigidBodyBundle},
    prelude::{
        ActiveEvents, ColliderHandle, ColliderShape, ContactEvent, RigidBodyMassProps,
        RigidBodyMassPropsFlags, RigidBodyPosition, RigidBodyVelocity,
    },
    render::ColliderDebugRender,
};

use serde::Deserialize;

use crate::{
    config_from_file, utils::TransformExt, GameStickDirectionalInput, KeyboardDirectionalInput,
    PlayerToSpawn, Yaw,
};

pub struct CharacterPlugin;

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_system(move_character_based_on_keyboard_input.system())
            .add_system_to_stage(
                CoreStage::PostUpdate,
                rotate_character_based_on_mouse_input.system(),
            )
            .add_system(touching_ground.system())
            .add_event::<CharacterToSpawn>()
            .add_system(spawn.system())
            .init_resource::<Config>();
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

#[derive(Deserialize, Clone)]
struct Config {
    size: f32,
    name: String,
    max_speed: f32,
    jump_force: f32,
}

impl Default for Config {
    fn default() -> Self {
        config_from_file!("character.ron")
    }
}

pub struct CharacterToSpawn {
    pub camera: Option<Entity>,
}

fn spawn(
    mut commands: Commands,
    mut characters_to_spawn: EventReader<CharacterToSpawn>,
    mut players_to_spawn: EventWriter<PlayerToSpawn>,
    config: Res<Config>,
) {
    for character_to_spawn in characters_to_spawn.iter() {
        let entity = commands
            .spawn()
            .insert_bundle(RigidBodyBundle {
                body_type: bevy_rapier3d::prelude::RigidBodyType::Dynamic,
                position: [0.0, 10., 0.].into(),
                mass_properties: RigidBodyMassProps {
                    flags: RigidBodyMassPropsFlags::ROTATION_LOCKED,
                    ..Default::default()
                },
                ..Default::default()
            })
            .insert_bundle(ColliderBundle {
                shape: ColliderShape::ball(config.size / 2.0),
                flags: (ActiveEvents::INTERSECTION_EVENTS | ActiveEvents::CONTACT_EVENTS).into(),
                ..Default::default()
            })
            .insert_bundle((
                MoveSpeed(config.max_speed),
                JumpForce(config.jump_force),
                Name(config.name.clone()),
                Touching::default(),
                ColliderDebugRender::with_id(0),
                ColliderPositionSync::Discrete,
            ))
            .id();

        if let Some(camera) = character_to_spawn.camera {
            players_to_spawn.send(PlayerToSpawn {
                camera,
                size: config.size,
                character: entity,
            })
        }
    }
}

fn touching_ground(
    mut players: Query<(Entity, &mut Touching)>,
    mut events: EventReader<ContactEvent>,
) {
    // TODO: Simplify this block?
    for contact_event in events.iter() {
        for (player_entity, mut touching) in players.iter_mut() {
            match contact_event {
                ContactEvent::Stopped(handle1, handle2) => {
                    if player_entity == handle1.entity() {
                        if let Some(index) = touching.index(&handle2) {
                            touching.0.remove(index);
                        }
                    } else if player_entity == handle2.entity() {
                        if let Some(index) = touching.index(&handle1) {
                            touching.0.remove(index);
                        }
                    }
                }
                ContactEvent::Started(handle1, handle2) => {
                    if player_entity == handle1.entity() {
                        if let None = touching.index(&handle2) {
                            touching.0.push(*handle2);
                        }
                    } else if player_entity == handle2.entity() {
                        if let None = touching.index(&handle1) {
                            touching.0.push(*handle1);
                        }
                    }
                }
            }
        }
    }
}

fn move_character_based_on_keyboard_input(
    mut query: Query<(
        &mut KeyboardDirectionalInput,
        &GameStickDirectionalInput,
        &Transform,
        &MoveSpeed,
        &JumpForce,
        &Touching,
        &mut RigidBodyVelocity,
        &RigidBodyMassProps,
    )>,
) {
    for (
        mut keyboard_directional_input,
        gamepad_directional_input,
        transform,
        move_speed,
        jump_force,
        touch_tracker,
        mut velocity,
        mass_properties,
    ) in query.iter_mut()
    {
        //
        // Get the current velocity from the physics engine
        //
        let current_velocity = velocity.linvel.clone_owned();

        //
        // Combine the keyboard and gamepad directional inputs
        //
        let mut combined_directional_input = Vec3::ZERO;
        combined_directional_input.x =
            keyboard_directional_input.0.x + gamepad_directional_input.0.x;
        combined_directional_input.y =
            keyboard_directional_input.0.y + gamepad_directional_input.0.y;
        combined_directional_input.z =
            keyboard_directional_input.0.z + gamepad_directional_input.0.z;
        if combined_directional_input != Vec3::ZERO {
            combined_directional_input.normalize();
        }

        // Now that we've read this, reset it so it can be summed up again next frame
        keyboard_directional_input.0 = Vec3::ZERO;

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
            let right = transform.right() * combined_directional_input.x;
            let desired_horizontal_velocity = Vec3::from(forward + right) * move_speed.0;

            //
            // get a copy of the current velocity from rapier, isolated to horizontal components only
            // (ie, zero out current vertical [y] component)
            //
            let current_horizontal_velocity =
                Vec3::new(current_velocity[(0, 0)], 0.0, current_velocity[(2, 0)]);

            //
            // To move the character, we increase the speed to match the maximum speed in whatever
            // direction is indicated by user keypress; or, if no keys pressed then we cancel out
            // any velocity to stop horizontally.
            //
            let horizontal_velocity_change =
                match desired_horizontal_velocity.abs().max_element() > 0.0 {
                    true => {
                        let current_speed_along_propulsion_direction = current_velocity
                            .dot(&desired_horizontal_velocity.normalize().into())
                            .abs();
                        let current_velocity_along_propulsion_direction =
                            match current_horizontal_velocity.abs().max_element() > 0.0 {
                                true => {
                                    current_speed_along_propulsion_direction
                                        * current_horizontal_velocity.normalize()
                                }
                                false => Vec3::ZERO,
                            };
                        desired_horizontal_velocity - current_velocity_along_propulsion_direction
                    }
                    false => -current_horizontal_velocity,
                };

            // Apply the computed impulse to the character's rigid body
            let mut horizontal_impulse = mass_properties.mass() * horizontal_velocity_change * 0.13; // slowing down with fudge factor

            if !touch_tracker.touching() {
                horizontal_impulse *= 0.13; // slowing down even more when in air
            }

            velocity.apply_impulse(mass_properties, horizontal_impulse.into());
        }

        // TODO: Update this documentation and variable names,
        // since we're doing an impulse instead of force now.
        //
        // Now consider the vertical plane (y)
        // Compute our desired vertical velocity vector and apply a force to the rigid body.
        //
        if touch_tracker.touching() && combined_directional_input.y != 0. {
            //
            // Compute our desired vertical velocity vector based on keyboard inputs and move speed
            //  Note: Horizontal plane = (x,z), Vertical plane = (y)
            //
            //  Note: We presume that keyboard directional input is limited externally.  If not,
            //          then a long keypress will act more like "thrust" upwards than singular
            //          jump event.
            //
            let up = transform.up() * combined_directional_input.y;
            let desired_vertical_velocity = Vec3::from(up) * jump_force.0;

            //
            // get a copy of the current velocity from rapier, isolated to vertical component only
            // (ie, zero out current horizontal [x,z] components)
            //
            let current_vertical_velocity = Vec3::new(0.0, current_velocity[(1, 0)], 0.0);

            //
            // To "jump" we allow apply force in the vertical direction
            //
            let vertical_velocity = match desired_vertical_velocity.abs().max_element() > 0.0 {
                true => {
                    let current_speed_along_propulsion_direction = current_velocity
                        .dot(&desired_vertical_velocity.normalize().into())
                        .abs();
                    let current_velocity_along_propulsion_direction =
                        match current_vertical_velocity.abs().max_element() > 0.0 {
                            true => {
                                current_speed_along_propulsion_direction
                                    * current_vertical_velocity.normalize()
                            }
                            false => Vec3::ZERO,
                        };
                    desired_vertical_velocity - current_velocity_along_propulsion_direction
                }
                false => Vec3::ZERO,
            };

            //
            // Apply the computed force to the character's rigid body
            //
            let vertical_force = mass_properties.mass() * vertical_velocity;
            velocity.apply_impulse(mass_properties, vertical_force.into());
        }
    }
}

fn rotate_character_based_on_mouse_input(
    mut query: Query<(&Transform, &Yaw, &mut RigidBodyPosition)>,
) {
    for (transform, yaw, mut position) in query.iter_mut() {
        let rotation = UnitQuaternion::from_axis_angle(&Vector3::y_axis(), -yaw.0);
        let new_position = Isometry::from_parts(transform.translation.into(), rotation);
        position.position = new_position;
    }
}
