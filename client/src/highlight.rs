use crate::render_main_pass::metal_material::MetalMaterial;
use bad_spaceship_shared::{part::RocketEngine, Focused};
use bevy::prelude::*;

pub struct HighlightPlugin;

impl Plugin for HighlightPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                focused_add_highlight,
                focused_remove_highlight,
                rocket_focus_add_highlight,
                rocket_focus_remove_highlight,
            ),
        );
    }
}

#[derive(Component)]
struct FocusedHighlight {
    base_color: Color,
}

fn focused_add_highlight(
    mut commands: Commands,
    newly_focused: Query<
        (Entity, &MeshMaterial3d<MetalMaterial>),
        (
            With<Focused>,
            Without<FocusedHighlight>,
            // Rockets glow via the metal shader's highlight tint (see below) so the
            // whole striped body lights up, not just its coloured bands.
            Without<RocketEngine>,
        ),
    >,
    mut materials: ResMut<Assets<MetalMaterial>>,
) {
    for (entity, material_handle) in newly_focused.iter() {
        let color = &mut materials.get_mut(material_handle.id()).unwrap().base.base_color;
        commands.entity(entity).insert(FocusedHighlight {
            base_color: *color,
        });

        // Make yellow. Bevy 0.14 dropped `Color`'s per-channel setters, so build the
        // highlight colour outright (preserving the original alpha).
        *color = Color::srgba(1.0, 1.0, 0.0, color.alpha());
    }
}

fn focused_remove_highlight(
    mut commands: Commands,
    higlighted: Query<
        (Entity, &MeshMaterial3d<MetalMaterial>, &FocusedHighlight),
        Without<Focused>,
    >,
    mut materials: ResMut<Assets<MetalMaterial>>,
) {
    for (entity, material_handle, highlight) in higlighted.iter() {
        let color = &mut materials.get_mut(material_handle.id()).unwrap().base.base_color;
        *color = highlight.base_color;
        commands.entity(entity).remove::<FocusedHighlight>();
    }
}

/// Marks a rocket currently glowing (focus highlight) via the metal shader's
/// `highlight` tint, so `rocket_focus_remove_highlight` knows to clear it.
#[derive(Component)]
struct RocketFocusHighlight;

/// Rockets can't be uniformly recoloured by overwriting `base_color` — that only
/// tints the striped material's coloured bands and leaves the white ones (the "blue
/// stripes" bug) — so they're excluded from the cuboid recolour above and instead
/// drive the metal shader's whole-albedo `highlight` tint. Rockets glow yellow on
/// focus, like every other part.
fn rocket_focus_add_highlight(
    mut commands: Commands,
    newly_focused: Query<
        (Entity, &MeshMaterial3d<MetalMaterial>),
        (With<RocketEngine>, With<Focused>, Without<RocketFocusHighlight>),
    >,
    mut materials: ResMut<Assets<MetalMaterial>>,
) {
    for (entity, material_handle) in newly_focused.iter() {
        // Solid yellow (rgb), full strength (a). Pure 0/1 channels are identical in
        // sRGB and linear, so this matches the cuboid focus yellow after lighting.
        materials
            .get_mut(material_handle.id())
            .unwrap()
            .extension
            .set_highlight(Vec4::new(1.0, 1.0, 0.0, 1.0));
        commands.entity(entity).insert(RocketFocusHighlight);
    }
}

fn rocket_focus_remove_highlight(
    mut commands: Commands,
    unfocused: Query<
        (Entity, &MeshMaterial3d<MetalMaterial>),
        (With<RocketFocusHighlight>, Without<Focused>),
    >,
    mut materials: ResMut<Assets<MetalMaterial>>,
) {
    for (entity, material_handle) in unfocused.iter() {
        materials
            .get_mut(material_handle.id())
            .unwrap()
            .extension
            .set_highlight(Vec4::ZERO);
        commands.entity(entity).remove::<RocketFocusHighlight>();
    }
}
