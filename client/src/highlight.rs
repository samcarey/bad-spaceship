use crate::render_main_pass::metal_material::MetalMaterial;
use bad_spaceship_shared::Focused;
use bevy::prelude::*;

pub struct HighlightPlugin;

impl Plugin for HighlightPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (focus_add_highlight, focus_remove_highlight));
    }
}

/// The focus glow fed to the metal shader's `highlight` uniform: solid yellow (rgb),
/// full strength (a). Pure 0/1 channels are identical in sRGB and linear.
const FOCUS_TINT: Vec4 = Vec4::new(1.0, 1.0, 0.0, 1.0);

/// Marks a part currently lit by the focus highlight, so `focus_remove_highlight`
/// clears its shader tint once it loses `Focused`.
#[derive(Component)]
struct FocusHighlighted;

/// Light the focused part by mixing its whole albedo toward yellow via the metal
/// shader's `highlight` uniform. This is the single highlight mechanism for *every*
/// part — cuboids and rockets alike — so a striped rocket glows uniformly (bands
/// included) rather than only its coloured bands, and no `base_color` is overwritten
/// (nothing to save/restore; the tint resets to `Vec4::ZERO`).
fn focus_add_highlight(
    mut commands: Commands,
    newly_focused: Query<
        (Entity, &MeshMaterial3d<MetalMaterial>),
        (With<Focused>, Without<FocusHighlighted>),
    >,
    mut materials: ResMut<Assets<MetalMaterial>>,
) {
    for (entity, material_handle) in newly_focused.iter() {
        if let Some(mut material) = materials.get_mut(material_handle.id()) {
            material.extension.set_highlight(FOCUS_TINT);
        }
        commands.entity(entity).insert(FocusHighlighted);
    }
}

fn focus_remove_highlight(
    mut commands: Commands,
    unfocused: Query<
        (Entity, &MeshMaterial3d<MetalMaterial>),
        (With<FocusHighlighted>, Without<Focused>),
    >,
    mut materials: ResMut<Assets<MetalMaterial>>,
) {
    for (entity, material_handle) in unfocused.iter() {
        if let Some(mut material) = materials.get_mut(material_handle.id()) {
            material.extension.set_highlight(Vec4::ZERO);
        }
        commands.entity(entity).remove::<FocusHighlighted>();
    }
}
