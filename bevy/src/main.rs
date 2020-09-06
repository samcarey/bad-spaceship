use bevy::prelude::*;

mod plugins;

fn main() {
    App::build()
        .add_default_plugins()
        .add_plugin(plugins::MapPlugin)
        .add_plugin(plugins::PlayerPlugin)
        .run();
}

const TEST: i32 = 60;
const TEST2: i32 = 0;
