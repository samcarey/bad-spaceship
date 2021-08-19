use std::time::Duration;

use bevy::{app::ScheduleRunnerSettings, prelude::*};

fn main() {
    App::build()
        .insert_resource(ScheduleRunnerSettings::run_loop(Duration::from_secs_f64(
            1.0 / 60.,
        )))
        .add_plugins(MinimalPlugins)
        .run();
}
