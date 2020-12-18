use super::types::{self, *};
use bevy::{app::ScheduleRunnerSettings, prelude::*};
use bevy_networking_turbulence::NetworkResource;
use std::net::SocketAddr;
use std::time::Duration;

pub struct ServerPlugin;

impl Plugin for ServerPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_resource(ScheduleRunnerSettings::run_loop(Duration::from_secs_f64(
            1.0 / 60.0,
        )))
        .add_startup_system(server_setup.system())
        .add_system(ball_movement_system.system())
        .add_resource(NetworkBroadcast { frame: 0 })
        .add_system_to_stage(stage::PRE_UPDATE, handle_messages_server.system())
        .add_system_to_stage(stage::POST_UPDATE, network_broadcast_system.system());
    }
}

pub fn server_setup(mut net: ResMut<NetworkResource>) {
    let ip_address =
        bevy_networking_turbulence::find_my_ip_address().expect("can't find ip address");
    let socket_address = SocketAddr::new(ip_address, SERVER_PORT);
    log::info!("Starting server");
    net.listen(socket_address);
}

pub fn handle_messages_server(
    mut net: ResMut<NetworkResource>,
    mut balls: Query<(&mut Ball, &Pawn)>,
) {
    for (handle, connection) in net.connections.iter_mut() {
        let channels = connection.channels().unwrap();
        while let Some(client_message) = channels.recv::<ClientMessage>() {
            log::debug!(
                "ClientMessage received on [{}]: {:?}",
                handle,
                client_message
            );
            match client_message {
                ClientMessage::Hello(id) => {
                    log::info!("Client [{}] connected on [{}]", id, handle);
                    // TODO: store client id?
                }
                ClientMessage::Direction(dir) => {
                    let mut angle: f32 = 0.03;
                    if dir == types::Direction::Right {
                        angle *= -1.0;
                    }
                    for (mut ball, pawn) in balls.iter_mut() {
                        if pawn.controller == *handle {
                            ball.velocity = Quat::from_rotation_z(angle) * ball.velocity;
                        }
                    }
                }
            }
        }

        while let Some(_state_message) = channels.recv::<GameStateMessage>() {
            log::error!("GameStateMessage received on [{}]", handle);
        }
    }
}

pub fn network_broadcast_system(
    mut state: ResMut<NetworkBroadcast>,
    mut net: ResMut<NetworkResource>,
    ball_query: Query<(Entity, &Ball, &Transform)>,
) {
    let mut message = GameStateMessage {
        frame: state.frame,
        balls: Vec::new(),
    };
    state.frame += 1;

    for (entity, ball, transform) in ball_query.iter() {
        message
            .balls
            .push((entity.id(), ball.velocity, transform.translation));
    }

    net.broadcast_message(message);
}

pub fn ball_movement_system(time: Res<Time>, mut ball_query: Query<(&Ball, &mut Transform)>) {
    for (ball, mut transform) in ball_query.iter_mut() {
        let mut translation = transform.translation + (ball.velocity * time.delta_seconds);
        let mut x = translation.x() as i32 % BOARD_WIDTH as i32;
        let mut y = translation.y() as i32 % BOARD_HEIGHT as i32;
        if x < 0 {
            x += BOARD_WIDTH as i32;
        }
        if y < 0 {
            y += BOARD_HEIGHT as i32;
        }
        translation.set_x(x as f32);
        translation.set_y(y as f32);
        transform.translation = translation;
    }
}
