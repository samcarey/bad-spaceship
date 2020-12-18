use bevy::{app::PluginGroupBuilder, prelude::*};
use bevy_networking_turbulence::{
    ConnectionChannelsBuilder, MessageChannelMode, MessageChannelSettings, NetworkEvent,
    NetworkResource, NetworkingPlugin, ReliableChannelSettings,
};
use rand::Rng;
// use serde::Deserialize;
use std::time::Duration;

pub mod client;
pub mod server;
mod types;
use super::super::utils::Args;

// use config_from_file_macro::ConfigFromFileMacro;
// use config_from_file_macro_derive::ConfigFromFileMacro;
use types::*;

// const CONFIG_FILE: &str = "assets/config/network.ron";

// #[derive(ConfigFromFileMacro, Deserialize)]
// struct Config {
//     server_ip_address: String,
//     server_port: u16,
// }

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
    mut commands: Commands,
    mut net: ResMut<NetworkResource>,
    mut state: ResMut<NetworkReader>,
    args: Res<Args>,
    network_events: Res<Events<NetworkEvent>>,
) {
    for event in state.network_events.iter(&network_events) {
        match event {
            NetworkEvent::Connected(handle) => match net.connections.get_mut(handle) {
                Some(connection) => {
                    match connection.remote_address() {
                        Some(remote_address) => {
                            log::debug!(
                                "Incoming connection on [{}] from [{}]",
                                handle,
                                remote_address
                            );

                            // New client connected - spawn a ball
                            let mut rng = rand::thread_rng();
                            let vel_x = rng.gen_range(-0.5, 0.5);
                            let vel_y = rng.gen_range(-0.5, 0.5);
                            let pos_x = rng.gen_range(0.0, BOARD_WIDTH as f32);
                            let pos_y = rng.gen_range(0.0, BOARD_HEIGHT as f32);
                            log::info!("Spawning {}x{} {}/{}", pos_x, pos_y, vel_x, vel_y);
                            commands.spawn((
                                Ball {
                                    velocity: 400.0 * Vec3::new(vel_x, vel_y, 0.0).normalize(),
                                },
                                Pawn {
                                    controller: *handle,
                                },
                                Transform::from_translation(Vec3::new(pos_x, pos_y, 1.0)),
                            ));
                        }
                        None => {
                            log::debug!("Connected on [{}]", handle);
                        }
                    }

                    if !args.is_server {
                        log::debug!("Sending Hello on [{}]", handle);
                        match net.send_message(*handle, ClientMessage::Hello("test".to_string())) {
                            Ok(msg) => match msg {
                                Some(msg) => {
                                    log::error!("Unable to send Hello: {:?}", msg);
                                }
                                None => {}
                            },
                            Err(err) => {
                                log::error!("Unable to send Hello: {:?}", err);
                            }
                        };
                    }
                }
                None => panic!("Got packet for non-existing connection [{}]", handle),
            },
            _ => {}
        }
    }
}

const CLIENT_STATE_MESSAGE_SETTINGS: MessageChannelSettings = MessageChannelSettings {
    channel: 0,
    channel_mode: MessageChannelMode::Reliable {
        reliability_settings: ReliableChannelSettings {
            bandwidth: 4096,
            recv_window_size: 1024,
            send_window_size: 1024,
            burst_bandwidth: 1024,
            init_send: 512,
            wakeup_time: Duration::from_millis(100),
            initial_rtt: Duration::from_millis(200),
            max_rtt: Duration::from_secs(2),
            rtt_update_factor: 0.1,
            rtt_resend_factor: 1.5,
        },
        max_message_len: 1024,
    },
    message_buffer_size: 8,
    packet_buffer_size: 8,
};

const GAME_STATE_MESSAGE_SETTINGS: MessageChannelSettings = MessageChannelSettings {
    channel: 1,
    channel_mode: MessageChannelMode::Unreliable,
    message_buffer_size: 8,
    packet_buffer_size: 8,
};

// #[derive(Default)]
// struct Online(bool);

// struct GameState {
//     frame: u32,
//     player_transforms: Vec<Transform>,
// }
