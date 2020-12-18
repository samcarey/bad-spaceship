use super::types::{self, *};
use bevy::{prelude::*, render::camera::WindowOrigin};
use bevy_networking_turbulence::NetworkResource;
use std::{collections::HashMap, net::SocketAddr};

pub struct ClientPlugin;

impl Plugin for ClientPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_resource(WindowDescriptor {
            width: BOARD_WIDTH,
            height: BOARD_HEIGHT,
            ..Default::default()
        })
        .add_startup_system(setup.system())
        .add_system_to_stage(stage::PRE_UPDATE, handle_messages.system())
        .add_resource(ServerIds::default())
        .add_system(ball_control_system.system());
        // app.init_resource::<Online>();
    }
}

pub fn setup(mut commands: Commands, mut net: ResMut<NetworkResource>) {
    let mut camera = Camera2dComponents::default();
    camera.orthographic_projection.window_origin = WindowOrigin::BottomLeft;
    commands.spawn(camera);

    let ip_address =
        bevy_networking_turbulence::find_my_ip_address().expect("can't find ip address");
    let socket_address = SocketAddr::new(ip_address, SERVER_PORT);
    log::info!("Starting client");
    net.connect(socket_address);
}

pub fn handle_messages(
    mut commands: Commands,
    mut net: ResMut<NetworkResource>,
    mut server_ids: ResMut<ServerIds>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut balls: Query<(Entity, &mut Ball, &mut Transform)>,
) {
    for (handle, connection) in net.connections.iter_mut() {
        let channels = connection.channels().unwrap();
        while let Some(_client_message) = channels.recv::<ClientMessage>() {
            log::error!("ClientMessage received on [{}]", handle);
        }

        // it is possible that many state updates came at the same time - spawn once
        let mut to_spawn: HashMap<u32, (u32, Vec3, Vec3)> = HashMap::new();

        while let Some(mut state_message) = channels.recv::<GameStateMessage>() {
            let message_frame = state_message.frame;
            log::info!(
                "GameStateMessage received on [{}]: {:?}",
                handle,
                state_message
            );

            // update all balls
            for (entity, mut ball, mut transform) in balls.iter_mut() {
                let server_id_entry = server_ids.get_mut(&entity.id()).unwrap();
                let (server_id, update_frame) = *server_id_entry;

                if let Some(index) = state_message
                    .balls
                    .iter()
                    .position(|&update| update.0 == server_id)
                {
                    let (_id, velocity, translation) = state_message.balls.remove(index);

                    if update_frame > message_frame {
                        continue;
                    }
                    server_id_entry.1 = message_frame;

                    ball.velocity = velocity;
                    transform.translation = translation;
                } else {
                    // TODO: despawn disconnected balls
                }
            }
            // create new balls
            for (id, velocity, translation) in state_message.balls.drain(..) {
                if let Some((frame, _velocity, _translation)) = to_spawn.get(&id) {
                    if *frame > message_frame {
                        continue;
                    }
                };
                to_spawn.insert(id, (message_frame, velocity, translation));
            }
        }

        for (id, (frame, velocity, translation)) in to_spawn.iter() {
            log::info!("Spawning {} @{}", id, frame);
            let entity = commands
                .spawn(SpriteComponents {
                    material: materials.add(
                        Color::rgb(0.8 - (*id as f32 / 5.0), 0.2, 0.2 + (*id as f32 / 5.0)).into(),
                    ),
                    transform: Transform::from_translation(*translation),
                    sprite: Sprite::new(Vec2::new(30.0, 30.0)),
                    ..Default::default()
                })
                .with(Ball {
                    velocity: *velocity,
                })
                .with(Pawn { controller: *id })
                .current_entity()
                .unwrap();
            server_ids.insert(entity.id(), (*id, *frame));
        }
    }
}

pub type ServerIds = HashMap<u32, (u32, u32)>;

pub fn ball_control_system(mut net: ResMut<NetworkResource>, keyboard_input: Res<Input<KeyCode>>) {
    if keyboard_input.pressed(KeyCode::Left) {
        net.broadcast_message(ClientMessage::Direction(types::Direction::Left));
    }

    if keyboard_input.pressed(KeyCode::Right) {
        net.broadcast_message(ClientMessage::Direction(types::Direction::Right));
    }
}
