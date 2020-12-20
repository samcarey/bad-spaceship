use super::types::*;
use bevy::prelude::*;
use bevy_networking_turbulence::NetworkResource;
use std::net::SocketAddr;

pub struct ServerPlugin;

impl Plugin for ServerPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_startup_system(setup.system())
            .add_resource(NetworkBroadcast { frame: 0 })
            .add_resource(GameState::default())
            .add_system_to_stage(stage::PRE_UPDATE, handle_messages.system())
            .add_system_to_stage(stage::POST_UPDATE, broadcast_game_state.system())
            .add_system(spawn.system());
    }
}

pub fn setup(mut net: ResMut<NetworkResource>) {
    let ip_address =
        bevy_networking_turbulence::find_my_ip_address().expect("Could not find IP address");
    let socket_address = SocketAddr::new(ip_address, SERVER_PORT);
    log::info!("Starting server");
    net.listen(socket_address);
}

pub fn spawn(mut commands: Commands) {
    commands.spawn(SerializablePlayer {
        id: 0,
        transform: Transform::default().compute_matrix(),
    });
}

pub fn handle_messages(mut net: ResMut<NetworkResource>, mut game_state: ResMut<GameState>) {
    for (handle, connection) in net.connections.iter_mut() {
        let channels = connection.channels().unwrap();
        println!("{:?}", channels.statistics::<ClientMessage>());
        while let Some(client_message) = channels.recv::<ClientMessage>() {
            log::debug!(
                "ClientMessage received on [{}]: {:?}",
                handle,
                client_message
            );
            println!("Received from client");
            if let Some(player) = game_state
                .players
                .iter_mut()
                .filter(|p| p.id == *handle)
                .next()
            {
                println!("{:?}", player);
                *player = client_message.player;
            } else {
                println!("Spawning");
                game_state.players.push(SerializablePlayer {
                    id: handle.clone(),
                    transform: client_message.player.transform,
                });
            }
        }
    }
}

pub fn broadcast_game_state(
    mut state: ResMut<NetworkBroadcast>,
    mut net: ResMut<NetworkResource>,
    game_state: ResMut<GameState>,
) {
    net.broadcast_message(GameStateMessage {
        frame: state.frame,
        game_state: GameState {
            players: game_state.players.clone(),
        },
    });
    state.frame += 1;
}
