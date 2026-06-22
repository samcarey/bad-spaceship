use bad_spaceship_shared::{Attachable, Focused};
use bevy::prelude::*;

pub struct HighlightPlugin;

impl Plugin for HighlightPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                focused_add_highlight,
                attachable_remove_highlight,
                attacheable_add_highlight,
                focused_remove_highlight,
            ),
        );
    }
}

#[derive(Component)]
struct AttachableHighlight {
    base_color: Color,
}

#[derive(Component)]
struct FocusedHighlight {
    base_color: Color,
}

fn attacheable_add_highlight(
    mut commands: Commands,
    attachables: Query<
        (Entity, &Handle<StandardMaterial>),
        (
            With<Attachable>,
            Without<AttachableHighlight>,
            Without<FocusedHighlight>,
        ),
    >,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (entity, material_handle) in attachables.iter() {
        let color = &mut materials.get_mut(&*material_handle).unwrap().base_color;
        commands.entity(entity).insert(AttachableHighlight {
            base_color: *color,
        });

        // Make turquoise. Bevy 0.14 dropped `Color`'s per-channel setters, so
        // build the highlight colour outright (preserving the original alpha).
        *color = Color::srgba(0.0, 1.0, 1.0, color.alpha());
    }
}

fn focused_add_highlight(
    mut commands: Commands,
    newly_focused: Query<
        (Entity, &Handle<StandardMaterial>),
        (
            With<Focused>,
            Without<FocusedHighlight>,
            Without<AttachableHighlight>,
        ),
    >,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (entity, material_handle) in newly_focused.iter() {
        let color = &mut materials.get_mut(&*material_handle).unwrap().base_color;
        commands.entity(entity).insert(FocusedHighlight {
            base_color: *color,
        });

        // Make yellow (preserving the original alpha; see Bevy 0.14 note above).
        *color = Color::srgba(1.0, 1.0, 0.0, color.alpha());
    }
}

fn focused_remove_highlight(
    mut commands: Commands,
    higlighted: Query<(Entity, &Handle<StandardMaterial>, &FocusedHighlight), Without<Focused>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (entity, material_handle, highlight) in higlighted.iter() {
        let color = &mut materials.get_mut(&*material_handle).unwrap().base_color;
        *color = highlight.base_color;
        commands.entity(entity).remove::<FocusedHighlight>();
    }
}

fn attachable_remove_highlight(
    mut commands: Commands,
    higlighted: Query<
        (Entity, &Handle<StandardMaterial>, &AttachableHighlight),
        Without<Attachable>,
    >,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (entity, material_handle, highlight) in higlighted.iter() {
        let color = &mut materials.get_mut(&*material_handle).unwrap().base_color;
        *color = highlight.base_color;
        commands.entity(entity).remove::<AttachableHighlight>();
    }
}
