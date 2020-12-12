use bevy::prelude::*;
use bevy::ui::prelude::ButtonComponents;

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.init_resource::<ButtonMaterials>()
            .init_resource::<MenuState>()
            .add_startup_system(setup.system())
            .add_system(toggle_menu_state.system())
            .add_system(hide_cursor.system())
            .add_system(button_system.system());
    }
}

#[derive(PartialEq)]
pub enum MenuState {
    Open,
    Closed,
}

impl Default for MenuState {
    fn default() -> Self {
        MenuState::Closed
    }
}

struct ButtonMaterials {
    normal: Handle<ColorMaterial>,
    hovered: Handle<ColorMaterial>,
    pressed: Handle<ColorMaterial>,
}

impl FromResources for ButtonMaterials {
    fn from_resources(resources: &Resources) -> Self {
        let mut materials = resources.get_mut::<Assets<ColorMaterial>>().unwrap();
        ButtonMaterials {
            normal: materials.add(Color::rgb(0.15, 0.15, 0.15).into()),
            hovered: materials.add(Color::rgb(0.25, 0.25, 0.25).into()),
            pressed: materials.add(Color::rgb(0.35, 0.75, 0.35).into()),
        }
    }
}

fn button_system(
    button_materials: Res<ButtonMaterials>,
    mut interaction_query: Query<(
        &Button,
        Mutated<Interaction>,
        &mut Handle<ColorMaterial>,
        &Children,
    )>,
    mut text_query: Query<&mut Text>,
) {
    for (_button, interaction, mut material, children) in interaction_query.iter_mut() {
        let mut text = text_query.get_mut(children[0]).unwrap();
        match *interaction {
            Interaction::Clicked => {
                text.value = "Press".to_string();
                *material = button_materials.pressed.clone();
            }
            Interaction::Hovered => {
                text.value = "Hover".to_string();
                *material = button_materials.hovered.clone();
            }
            Interaction::None => {
                text.value = "Button".to_string();
                *material = button_materials.normal.clone();
            }
        }
    }
}

fn setup(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    button_materials: Res<ButtonMaterials>,
) {
    commands
        // ui camera
        .spawn(UiCameraComponents::default())
        .spawn(ButtonComponents {
            style: Style {
                size: Size::new(Val::Px(150.0), Val::Px(65.0)),
                // center button
                margin: Rect::all(Val::Auto),
                // horizontally center child text
                justify_content: JustifyContent::Center,
                // vertically center child text
                align_items: AlignItems::Center,
                ..Default::default()
            },
            material: button_materials.normal.clone(),
            ..Default::default()
        })
        .with_children(|parent| {
            parent.spawn(TextComponents {
                text: Text {
                    value: "Button".to_string(),
                    font: asset_server.load("fonts/FiraSans-Bold.ttf"),
                    style: TextStyle {
                        font_size: 40.0,
                        color: Color::rgb(0.9, 0.9, 0.9),
                    },
                },
                ..Default::default()
            });
        });
}

fn toggle_menu_state(input: Res<Input<KeyCode>>, mut menu_state: ResMut<MenuState>) {
    if input.just_pressed(KeyCode::Escape) {
        *menu_state = match *menu_state {
            MenuState::Closed => MenuState::Open,
            MenuState::Open => MenuState::Closed,
        }
    }
}

fn hide_cursor(menu_state: ResMut<MenuState>, mut windows: ResMut<Windows>) {
    let window = windows.get_primary_mut().unwrap();
    match *menu_state {
        MenuState::Closed => {
            window.set_cursor_lock_mode(true);
            window.set_cursor_visibility(false);
        }
        MenuState::Open => {
            window.set_cursor_lock_mode(false);
            window.set_cursor_visibility(true);
        }
    }
}
