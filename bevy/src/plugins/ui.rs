use bevy::prelude::*;
use bevy::ui::prelude::ButtonComponents;
use config_from_file_macro::ConfigFromFileMacro;
use config_from_file_macro_derive::ConfigFromFileMacro;
use serde::Deserialize;

const CONFIG_FILE: &str = "assets/config/ui.ron";

#[derive(ConfigFromFileMacro, Deserialize)]
struct Config {
    menu_button_height: f32,
    menu_button_width: f32,
    menu_button_spacing: f32,
}

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.init_resource::<ButtonMaterials>()
            .init_resource::<MenuState>()
            .add_event::<Selection>()
            .add_startup_system(spawn_camera.system())
            .add_system(toggle_menu_state.system())
            .add_system(toggle_menu.system())
            .add_system(hide_cursor.system())
            .add_system(button_interaction.system())
            .add_system(selection_event_listener.system())
            .add_system(resume.system());
    }
}

struct Menu;

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
            pressed: materials.add(Color::rgb(0.35, 0.35, 0.35).into()),
        }
    }
}

fn button_interaction(
    button_materials: Res<ButtonMaterials>,
    mut selection_events: ResMut<Events<Selection>>,
    _button: &Button,
    selection: &Selection,
    interaction: Mutated<Interaction>,
    mut material: Mut<Handle<ColorMaterial>>,
) {
    match *interaction {
        Interaction::Clicked => {
            *material = button_materials.pressed.clone();
            selection_events.send(*selection);
        }
        Interaction::Hovered => {
            *material = button_materials.hovered.clone();
        }
        Interaction::None => {
            *material = button_materials.normal.clone();
        }
    }
}

fn resume(
    mut menu_state: ResMut<MenuState>,
    mut selection_event_reader: Local<EventReader<Selection>>,
    selection_events: Res<Events<Selection>>,
) {
    for selection in selection_event_reader.iter(&selection_events) {
        if *selection == Selection::Resume {
            *menu_state = MenuState::Closed;
        }
    }
}

fn selection_event_listener(
    mut my_event_reader: Local<EventReader<Selection>>,
    my_events: Res<Events<Selection>>,
) {
    for my_event in my_event_reader.iter(&my_events) {
        println!("{:?}", my_event);
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Selection {
    Resume,
    Options,
    Multiplayer,
}

fn spawn_camera(mut commands: Commands) {
    commands.spawn(UiCameraComponents::default());
}

fn spawn_menu(
    commands: &mut Commands,
    asset_server: Res<AssetServer>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    button_materials: Res<ButtonMaterials>,
) {
    let config: Config = Config::new(CONFIG_FILE);
    let names = ["Options", "Multiplayer", "Resume"];
    let menu_options = [
        Selection::Multiplayer,
        Selection::Options,
        Selection::Resume,
    ];
    let offset_increment = config.menu_button_height + config.menu_button_spacing;
    let menu_items_height = config.menu_button_height * names.len() as f32
        + config.menu_button_spacing * (names.len() - 1) as f32;

    commands
        // root menu node
        .spawn(NodeComponents {
            style: Style {
                size: Size::new(Val::Percent(100.0), Val::Percent(100.0)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..Default::default()
            },
            material: materials.add(Color::NONE.into()),
            ..Default::default()
        })
        .with(Menu)
        .with_children(|parent| {
            parent
                // button portion of menu
                .spawn(NodeComponents {
                    style: Style {
                        size: Size::new(
                            Val::Px(config.menu_button_width),
                            Val::Px(menu_items_height),
                        ),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::FlexEnd,
                        ..Default::default()
                    },
                    material: materials.add(Color::NONE.into()),
                    ..Default::default()
                })
                .with_children(move |parent| {
                    // buttons
                    for (i, selection) in menu_options.iter().enumerate() {
                        parent
                            .spawn(ButtonComponents {
                                style: Style {
                                    size: Size::new(Val::Percent(100.0), Val::Px(65.0)),
                                    // center button
                                    margin: Rect::all(Val::Auto),
                                    // horizontally center child text
                                    justify_content: JustifyContent::Center,
                                    // vertically center child text
                                    align_items: AlignItems::Center,
                                    position_type: PositionType::Absolute,
                                    position: Rect {
                                        top: Val::Px(i as f32 * offset_increment),
                                        ..Default::default()
                                    },
                                    ..Default::default()
                                },
                                material: button_materials.normal.clone(),
                                ..Default::default()
                            })
                            .with(selection.clone())
                            .with_children(|parent| {
                                parent.spawn(TextComponents {
                                    text: Text {
                                        value: format!("{:?}", selection),
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
                });
        });
}

fn toggle_menu(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    materials: ResMut<Assets<ColorMaterial>>,
    button_materials: Res<ButtonMaterials>,
    menu_state: ChangedRes<MenuState>,
    menu_query: Query<(Entity, &Menu)>,
) {
    match *menu_state {
        MenuState::Open => {
            spawn_menu(&mut commands, asset_server, materials, button_materials);
        }
        MenuState::Closed => {
            for (menu_entity, _menu_component) in menu_query.iter() {
                commands.despawn_recursive(menu_entity);
            }
        }
    }
}

fn toggle_menu_state(input: ChangedRes<Input<KeyCode>>, mut menu_state: ResMut<MenuState>) {
    if input.just_pressed(KeyCode::Escape) {
        *menu_state = match *menu_state {
            MenuState::Closed => MenuState::Open,
            MenuState::Open => MenuState::Closed,
        }
    }
}

fn hide_cursor(menu_state: ChangedRes<MenuState>, mut windows: ResMut<Windows>) {
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
