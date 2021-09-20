use bad_spaceship_shared::{Attachable, Focused};
use bevy::prelude::*;

pub struct HighlightPlugin;

impl Plugin for HighlightPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_system(focused_add_highlight.system())
            .add_system(remove_highlight.system())
            .add_system(attacheable_add_highlight.system());
    }
}

struct Highlight {
    base_color: Color,
}

fn attacheable_add_highlight(
    mut commands: Commands,
    attachables: Query<(Entity, &Handle<StandardMaterial>), (With<Attachable>, Without<Highlight>)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (entity, material_handle) in attachables.iter() {
        let color = &mut materials.get_mut(&*material_handle).unwrap().base_color;
        commands.entity(entity).insert(Highlight {
            base_color: color.clone(),
        });

        // Make more turquiose
        color.set_g((color.g() + 0.75).min(1.0));
        color.set_b((color.b() + 0.75).min(1.0));
    }
}

fn focused_add_highlight(
    mut commands: Commands,
    newly_focused: Query<(Entity, &Handle<StandardMaterial>), (With<Focused>, Without<Highlight>)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (entity, material_handle) in newly_focused.iter() {
        let color = &mut materials.get_mut(&*material_handle).unwrap().base_color;
        commands.entity(entity).insert(Highlight {
            base_color: color.clone(),
        });

        // Make more yellowish
        color.set_g((color.g() + 0.75).min(1.0));
        color.set_r((color.r() + 0.75).min(1.0));
    }
}

fn remove_highlight(
    mut commands: Commands,
    higlighted: Query<
        (Entity, &Handle<StandardMaterial>, &Highlight),
        (Without<Focused>, Without<Attachable>),
    >,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (entity, material_handle, highlight) in higlighted.iter() {
        let color = &mut materials.get_mut(&*material_handle).unwrap().base_color;
        *color = highlight.base_color;
        commands.entity(entity).remove::<Highlight>();
    }
}
