use crate::AppState;
use bevy::input::mouse::MouseButtonInput;
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
            .init_resource::<TrackInputState>()
            .add_startup_system(spawn_camera.system())
            .add_system_set(
                SystemSet::on_enter(AppState::InGameMenu).with_system(spawn_menu.system()),
            )
            .add_system_set(
                SystemSet::on_exit(AppState::InGameMenu).with_system(despawn_menu.system()),
            )
            .add_system_set(
                SystemSet::on_update(AppState::InGameMenu)
                    .with_system(button_interaction.system())
                    .with_system(selection_listener.system())
                    .with_system(resume.system())
                    .with_system(options.system())
                    .with_system(multiplayer.system()),
            )
            .add_plugin(PlatformPlugin)
            .add_startup_system(spawn_diagnostics_text.system())
            .add_system(update_diagnostics_text.system())
            .add_system_set(
                SystemSet::on_update(AppState::Initial)
                    .with_system(capture_mouse_on_click.system()),
            )
            .add_startup_system(spawn_metadata_text.system());
    }
}

struct Menu;

struct ButtonMaterials {
    normal: Handle<ColorMaterial>,
    hovered: Handle<ColorMaterial>,
    pressed: Handle<ColorMaterial>,
}

impl FromWorld for ButtonMaterials {
    fn from_world(world: &mut World) -> Self {
        let mut materials = world.get_resource_mut::<Assets<ColorMaterial>>().unwrap();
        ButtonMaterials {
            normal: materials.add(Color::rgb(0.15, 0.15, 0.15).into()),
            hovered: materials.add(Color::rgb(0.25, 0.25, 0.25).into()),
            pressed: materials.add(Color::rgb(0.35, 0.35, 0.35).into()),
        }
    }
}

fn button_interaction(
    button_materials: Res<ButtonMaterials>,
    mut selection_events: EventWriter<Selection>,
    mut query: Query<
        (&Selection, &Interaction, &mut Handle<ColorMaterial>),
        (With<Button>, Changed<Interaction>),
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

fn resume(mut state: ResMut<State<AppState>>, mut selections: EventReader<Selection>) {
    for selection in selections.iter() {
        if *selection == Selection::Resume {
            state.set(AppState::InGame).unwrap();
        }
    }
}

fn options(mut selections: EventReader<Selection>) {
    for selection in selections.iter() {
        if *selection == Selection::Options {}
    }
}

fn multiplayer(mut selections: EventReader<Selection>) {
    for selection in selections.iter() {
        if *selection == Selection::Multiplayer {}
    }
}

fn selection_listener(mut selections: EventReader<Selection>) {
    for selection in selections.iter() {
        info!("Selected menu option: {:?}", selection);
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

fn spawn_camera(mut commands: Commands) {
    commands.spawn().insert_bundle(UiCameraBundle::default());
}

fn spawn_menu(
    mut commands: Commands,
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
        .spawn()
        .insert_bundle(NodeBundle {
            style: Style {
                size: Size::new(Val::Percent(100.0), Val::Percent(100.0)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..Default::default()
            },
            material: materials.add(Color::NONE.into()),
            ..Default::default()
        })
        .insert(Menu)
        .with_children(|parent| {
            parent
                // button portion of menu
                .spawn()
                .insert_bundle(NodeBundle {
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
                            .spawn()
                            .insert_bundle(ButtonBundle {
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
                            .insert(selection.clone())
                            .with_children(|parent| {
                                parent.spawn().insert_bundle(TextBundle {
                                    text: Text::with_section(
                                        format!("{:?}", selection),
                                        TextStyle {
                                            font: asset_server.load("fonts/FiraSans-Bold.ttf"),
                                            font_size: 40.0,
                                            color: Color::rgb(0.9, 0.9, 0.9),
                                        },
                                        TextAlignment::default(),
                                    ),
                                    ..Default::default()
                                });
                            });
                    }
                });
        });
}

fn despawn_menu(mut commands: Commands, menu_query: Query<Entity, With<Menu>>) {
    for menu_entity in menu_query.iter() {
        commands.entity(menu_entity).despawn_recursive();
    }
}

struct DiagnosticsText;

fn spawn_diagnostics_text(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands
        .spawn()
        .insert_bundle(TextBundle {
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
            text: Text::with_section(
                "This text changes in the bottom right".to_string(),
                TextStyle {
                    font: asset_server.load("fonts/FiraMono-Medium.ttf"),
                    font_size: 30.0,
                    color: Color::PINK,
                },
                TextAlignment::default(),
            ),
            ..Default::default()
        })
        .insert(DiagnosticsText);
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

        text.sections[0].value = format!("{:.0} FPS", fps,);
    }
}

#[derive(Default)]
struct TrackInputState {
    // mousebtn: EventReader<MouseButtonInput>,
}

fn capture_mouse_on_click(
    mut mouse_button_input_events: EventReader<MouseButtonInput>,
    // mut input_state: ResMut<TrackInputState>,
    mut state: ResMut<State<AppState>>,
) {
    for _ev in mouse_button_input_events.iter() {
        state.set(AppState::InGame).unwrap();
    }
}

fn spawn_metadata_text(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.spawn().insert_bundle(TextBundle {
        style: Style {
            align_self: AlignSelf::FlexEnd,
            position_type: PositionType::Absolute,
            position: Rect {
                bottom: Val::Px(5.0),
                left: Val::Px(15.0),
                ..Default::default()
            },
            ..Default::default()
        },
        text: Text::with_section(
            format!("v.{}", env!("CARGO_PKG_VERSION")),
            TextStyle {
                font: asset_server.load("fonts/FiraMono-Medium.ttf"),
                font_size: 30.0,
                color: Color::PINK,
            },
            TextAlignment::default(),
        ),
        ..Default::default()
    });
}
