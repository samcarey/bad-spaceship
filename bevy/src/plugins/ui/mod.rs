use crate::{AppState, APP_STATE};
use bevy::ui::prelude::ButtonBundle;
use bevy::{
    diagnostic::{Diagnostics, FrameTimeDiagnosticsPlugin},
    prelude::*,
};
use serde::Deserialize;

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
use native::PlatformPlugin;
#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
use web::PlatformPlugin;

#[derive(Deserialize)]
struct Config {
    menu_button_height: f32,
    menu_button_width: f32,
    menu_button_spacing: f32,
}

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_plugin(FrameTimeDiagnosticsPlugin)
            .init_resource::<ButtonMaterials>()
            .add_event::<Selection>()
            .add_startup_system(spawn_camera.system())
            .on_state_enter(APP_STATE, AppState::InGameMenu, spawn_menu.system())
            .on_state_update(APP_STATE, AppState::InGameMenu, close_menu_on_key.system())
            .on_state_exit(APP_STATE, AppState::InGameMenu, despawn_menu.system())
            .on_state_update(APP_STATE, AppState::InGameMenu, button_interaction.system())
            .on_state_update(APP_STATE, AppState::InGameMenu, selection_listener.system())
            .on_state_update(APP_STATE, AppState::InGameMenu, resume.system())
            .on_state_update(APP_STATE, AppState::InGameMenu, options.system())
            .on_state_update(APP_STATE, AppState::InGameMenu, multiplayer.system())
            .on_state_update(APP_STATE, AppState::InGame, open_menu_on_key.system())
            .add_plugin(PlatformPlugin)
            .add_startup_system(spawn_diagnostics_text.system())
            .add_system(update_diagnostics_text.system());
    }
}

struct Menu;

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
    mut query: Query<
        (&Selection, &Interaction, Mut<Handle<ColorMaterial>>),
        (With<Button>, Mutated<Interaction>),
    >,
) {
    for (selection, interaction, mut material) in query.iter_mut() {
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
}

fn resume(
    mut state: ResMut<State<AppState>>,
    mut selection_event_reader: Local<EventReader<Selection>>,
    selection_events: Res<Events<Selection>>,
) {
    for selection in selection_event_reader.iter(&selection_events) {
        if *selection == Selection::Resume {
            state.set_next(AppState::InGame).unwrap();
        }
    }
}

fn options(
    mut selection_event_reader: Local<EventReader<Selection>>,
    selection_events: Res<Events<Selection>>,
) {
    for selection in selection_event_reader.iter(&selection_events) {
        if *selection == Selection::Options {}
    }
}

fn multiplayer(
    mut selection_event_reader: Local<EventReader<Selection>>,
    selection_events: Res<Events<Selection>>,
) {
    for selection in selection_event_reader.iter(&selection_events) {
        if *selection == Selection::Multiplayer {}
    }
}

fn selection_listener(
    mut my_event_reader: Local<EventReader<Selection>>,
    my_events: Res<Events<Selection>>,
) {
    for my_event in my_event_reader.iter(&my_events) {
        println!("Selected menu option: {:?}", my_event);
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Selection {
    Resume,
    Options,
    Multiplayer,
}
const MENU_OPTIONS: [Selection; 3] = [
    Selection::Multiplayer,
    Selection::Options,
    Selection::Resume,
];

fn spawn_camera(commands: &mut Commands) {
    commands.spawn(CameraUiBundle::default());
}

fn spawn_menu(
    commands: &mut Commands,
    asset_server: Res<AssetServer>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    button_materials: Res<ButtonMaterials>,
) {
    let config: Config = config_from_file!("ui.ron");
    let offset_increment = config.menu_button_height + config.menu_button_spacing;
    let menu_items_height = config.menu_button_height * MENU_OPTIONS.len() as f32
        + config.menu_button_spacing * (MENU_OPTIONS.len() - 1) as f32;

    commands
        // root menu node
        .spawn(NodeBundle {
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
                .spawn(NodeBundle {
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
                    for (i, selection) in MENU_OPTIONS.iter().enumerate() {
                        parent
                            .spawn(ButtonBundle {
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
                                parent.spawn(TextBundle {
                                    text: Text {
                                        value: format!("{:?}", selection),
                                        font: asset_server.load("fonts/FiraSans-Bold.ttf"),
                                        style: TextStyle {
                                            font_size: 40.0,
                                            color: Color::rgb(0.9, 0.9, 0.9),
                                            alignment: TextAlignment::default(),
                                        },
                                    },
                                    ..Default::default()
                                });
                            });
                    }
                });
        });
}

fn despawn_menu(commands: &mut Commands, menu_query: Query<Entity, With<Menu>>) {
    for menu_entity in menu_query.iter() {
        commands.despawn_recursive(menu_entity);
    }
}

fn close_menu_on_key(input: ChangedRes<Input<KeyCode>>, mut state: ResMut<State<AppState>>) {
    if input.just_pressed(KeyCode::Escape) {
        state.set_next(AppState::InGame).unwrap();
    }
}

fn open_menu_on_key(input: ChangedRes<Input<KeyCode>>, mut state: ResMut<State<AppState>>) {
    if input.just_pressed(KeyCode::Escape) {
        state.set_next(AppState::InGameMenu).unwrap();
    }
}

struct DiagnosticsText;

fn spawn_diagnostics_text(commands: &mut Commands, asset_server: Res<AssetServer>) {
    commands
        .spawn(TextBundle {
            style: Style {
                align_self: AlignSelf::FlexEnd,
                position_type: PositionType::Absolute,
                position: Rect {
                    bottom: Val::Px(5.0),
                    right: Val::Px(15.0),
                    ..Default::default()
                },
                ..Default::default()
            },
            text: Text {
                value: "This text changes in the bottom right".to_string(),
                font: asset_server.load("fonts/FiraMono-Medium.ttf"),
                style: TextStyle {
                    font_size: 30.0,
                    color: Color::WHITE,
                    alignment: TextAlignment::default(),
                },
            },
            ..Default::default()
        })
        .with(DiagnosticsText);
}

fn update_diagnostics_text(
    diagnostics: Res<Diagnostics>,
    mut query: Query<&mut Text, With<DiagnosticsText>>,
) {
    for mut text in query.iter_mut() {
        let mut fps = 0.0;
        if let Some(fps_diagnostic) = diagnostics.get(FrameTimeDiagnosticsPlugin::FPS) {
            if let Some(fps_avg) = fps_diagnostic.average() {
                fps = fps_avg;
            }
        }

        text.value = format!("{:.0} FPS", fps,);
    }
}
