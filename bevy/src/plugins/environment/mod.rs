use bevy::{app::PluginGroupBuilder, prelude::*};

mod map;

pub struct EnvironmentPluginGroup;

impl PluginGroup for EnvironmentPluginGroup {
    fn build(&mut self, group: &mut PluginGroupBuilder) {
        group.add(map::MapPlugin);
    }
}
