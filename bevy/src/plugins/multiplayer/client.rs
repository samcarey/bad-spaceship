use super::super::player::Player;
use super::types::*;
use bevy::prelude::*;
use bevy_networking_turbulence::NetworkResource;
use std::net::SocketAddr;

pub struct ClientPlugin;

impl Plugin for ClientPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_startup_system(setup.system())
            .add_system_to_stage(stage::PRE_UPDATE, handle_messages.system())
            .add_system(ball_control_system.system());
    }
}

pub fn setup(mut net: ResMut<NetworkResource>) {
    let ip_address =
        bevy_networking_turbulence::find_my_ip_address().expect("can't find ip address");
    let socket_address = SocketAddr::new(ip_address, SERVER_PORT);
    log::info!("Starting client");
    net.connect(socket_address);
}

pub fn handle_messages(mut net: ResMut<NetworkResource>) {
    for (handle, connection) in net.connections.iter_mut() {
        let channels = connection.channels().unwrap();
        while let Some(state_message) = channels.recv::<GameStateMessage>() {
            log::info!(
                "GameStateMessage received on [{}]: {:?}",
                handle,
                state_message
            );
        }
    }
}

pub fn ball_control_system(
    mut net: ResMut<NetworkResource>,
    _player: &Player,
    transform: &Transform,
) {
    net.broadcast_message(ClientMessage {
        player: SerializablePlayer {
            id: 0,
            transform: transform.compute_matrix(),
        },
    });
}
