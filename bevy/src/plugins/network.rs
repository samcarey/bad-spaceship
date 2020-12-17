use bevy::prelude::*;
use bevy_networking_turbulence;
use config_from_file_macro::ConfigFromFileMacro;
use config_from_file_macro_derive::ConfigFromFileMacro;
use serde::Deserialize;

const CONFIG_FILE: &str = "assets/config/network.ron";

#[derive(ConfigFromFileMacro, Deserialize)]
struct Config {
    server_address: String,
}

pub struct NetworkPlugin;

impl Plugin for NetworkPlugin {
    fn build(&self, app: &mut AppBuilder) {}
}
