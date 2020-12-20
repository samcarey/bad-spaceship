use bevy::{app::PluginGroupBuilder, prelude::*};
use bevy_networking_turbulence::{
    ConnectionChannelsBuilder, MessageChannelMode, MessageChannelSettings, NetworkEvent,
    NetworkResource, NetworkingPlugin,
};
use types::*;

pub mod client;
pub mod server;
mod types;

pub struct MultiplayerPlugins;

impl PluginGroup for MultiplayerPlugins {
    fn build(&mut self, group: &mut PluginGroupBuilder) {
        group
            .add(server::ServerPlugin)
            .add(client::ClientPlugin)
            .add(CommonPlugin);
    }
}

struct CommonPlugin;

impl Plugin for CommonPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_plugin(NetworkingPlugin)
            .add_startup_system(network_setup.system())
            .add_resource(NetworkReader::default())
            .add_system(handle_packets.system());
    }
}

#[derive(Default)]
struct NetworkReader {
    network_events: EventReader<NetworkEvent>,
}

fn network_setup(mut net: ResMut<NetworkResource>) {
    net.set_channels_builder(|builder: &mut ConnectionChannelsBuilder| {
        builder
            .register::<ClientMessage>(CLIENT_STATE_MESSAGE_SETTINGS)
            .unwrap();
        builder
            .register::<GameStateMessage>(GAME_STATE_MESSAGE_SETTINGS)
            .unwrap();
    });
}

fn handle_packets(
    mut net: ResMut<NetworkResource>,
    mut state: ResMut<NetworkReader>,
    network_events: Res<Events<NetworkEvent>>,
) {
    for event in state.network_events.iter(&network_events) {
        if let NetworkEvent::Connected(handle) = event {
            let connection = net.connections.get_mut(handle).expect(&format!(
                "Got packet for non-existing connection [{}]",
                handle
            ));

            match connection.remote_address() {
                Some(remote_address) => {
                    log::debug!(
                        "Incoming connection on [{}] from [{}]",
                        handle,
                        remote_address
                    );
                }
                None => {
                    log::debug!("Connected on [{}]", handle);
                }
            }
        }
    }
}

const CLIENT_STATE_MESSAGE_SETTINGS: MessageChannelSettings = MessageChannelSettings {
    channel: 0,
    channel_mode: MessageChannelMode::Unreliable,
    message_buffer_size: 60,
    packet_buffer_size: 60,
};

const GAME_STATE_MESSAGE_SETTINGS: MessageChannelSettings = MessageChannelSettings {
    channel: 1,
    channel_mode: MessageChannelMode::Unreliable,
    message_buffer_size: 60,
    packet_buffer_size: 60,
};
