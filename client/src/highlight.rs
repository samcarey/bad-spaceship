use crate::render_main_pass::metal_material::MetalMaterial;
use bad_spaceship_shared::{part::RocketEngine, Attachable, Focused};
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
                rocket_focus_add_highlight,
                rocket_focus_remove_highlight,
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
        (Entity, &MeshMaterial3d<MetalMaterial>),
        (
            With<Attachable>,
            Without<AttachableHighlight>,
            Without<FocusedHighlight>,
            // Rockets are handled by the metal shader's highlight tint below (and
            // per the design, don't glow turquoise on attach at all).
            Without<RocketEngine>,
        ),
    >,
    mut materials: ResMut<Assets<MetalMaterial>>,
) {
    for (entity, material_handle) in attachables.iter() {
        let color = &mut materials.get_mut(material_handle.id()).unwrap().base.base_color;
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
        (Entity, &MeshMaterial3d<MetalMaterial>),
        (
            With<Focused>,
            Without<FocusedHighlight>,
            Without<AttachableHighlight>,
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

        // Make yellow (preserving the original alpha; see Bevy 0.14 note above).
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

fn attachable_remove_highlight(
    mut commands: Commands,
    higlighted: Query<
        (Entity, &MeshMaterial3d<MetalMaterial>, &AttachableHighlight),
        Without<Attachable>,
    >,
    mut materials: ResMut<Assets<MetalMaterial>>,
) {
    for (entity, material_handle, highlight) in higlighted.iter() {
        let color = &mut materials.get_mut(material_handle.id()).unwrap().base.base_color;
        *color = highlight.base_color;
        commands.entity(entity).remove::<AttachableHighlight>();
    }
}

/// Marks a rocket currently glowing (focus highlight) via the metal shader's
/// `highlight` tint, so `rocket_focus_remove_highlight` knows to clear it.
#[derive(Component)]
struct RocketFocusHighlight;

/// Rockets can't be uniformly recoloured by overwriting `base_color` — that only
/// tints the striped material's coloured bands and leaves the white ones (the "blue
/// stripes" bug) — so they're excluded from the cuboid recolour above and instead
/// drive the metal shader's whole-albedo `highlight` tint. Per the design choice,
/// rockets glow on **focus only** (solid yellow), never on attach (no turquoise).
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
