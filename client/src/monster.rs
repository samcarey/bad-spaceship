//! Every player is a blob-like alien monster from Quaternius' CC0 "Cute
//! Animated Monsters" pack (quaternius.com — 8 of the 21 ship, chosen for
//! alien-ness; the pack's flyers are excluded because they lack Idle/Walk
//! clips). The monster is *assigned*, not chosen: a hash of the player's
//! persistent resume id picks one of the 8 (`monster_index`, shared), so it
//! survives reload/reset on web, and in multiplayer the server replicates the
//! pick on `NetPlayer::monster` — every client sees you as the same monster,
//! and it's the same one you see in single-player.
//!
//! The visual is a glTF scene hung under the (invisible) capsule body: a yaw
//! pivot child (own body: rotated from `Yaw` each frame; remote avatars: the
//! existing `AvatarVisual` facing path) containing the scaled scene, feet at
//! the capsule's bottom. Models load lazily over HTTP, so a client only ever
//! downloads the ~300 KB monsters actually present, never all eight.

use std::time::Duration;

use bad_spaceship_shared::{
    net::{monster_index, NetMoving, MONSTER_COUNT},
    Character, DirectionalInput, Yaw,
};
use bevy::{
    gltf::GltfAssetLabel, prelude::*, world_serialization::WorldAssetRoot,
    world_serialization::WorldInstanceReady,
};

/// (asset path, uniform scale). The scale normalizes each model's rest-pose
/// height (measured from the glTF POSITION bounds) to the capsule's 1.5 m so
/// every monster stands exactly as tall as the body it dresses.
const MONSTERS: [(&str, f32); 8] = [
    ("monsters/Alien.glb", 1.5 / 2.06),
    ("monsters/Alien_Tall.glb", 1.5 / 2.11),
    ("monsters/Ghost.glb", 1.5 / 1.40),
    ("monsters/GreenDemon.glb", 1.5 / 1.62),
    ("monsters/Cyclops.glb", 1.5 / 1.67),
    ("monsters/Demon.glb", 1.5 / 1.62),
    ("monsters/Yeti.glb", 1.5 / 1.67),
    ("monsters/Mushroom.glb", 1.5 / 2.07),
];

// The assignment hash (shared, so the server agrees) reduces modulo
// MONSTER_COUNT; tie the table to it so adding a model without bumping the
// shared count is a build error instead of a silently unreachable monster.
const _: () = assert!(MONSTERS.len() == MONSTER_COUNT as usize);

/// The model filename stem for avatar `index` (e.g. `"Alien_Tall"`), sliced out of its
/// table path so the picker's names/thumbnails stay in lockstep with `MONSTERS`.
fn avatar_stem(index: u8) -> &'static str {
    let path = MONSTERS[index as usize % MONSTERS.len()].0; // "monsters/Alien_Tall.glb"
    let start = path.rfind('/').map_or(0, |i| i + 1);
    &path[start..path.len() - 4] // strip the directory prefix and the ".glb" suffix
}

/// Human-facing name for avatar `index` (e.g. `"Alien Tall"`), for the picker labels.
pub fn avatar_name(index: u8) -> String {
    avatar_stem(index).replace('_', " ")
}

/// Asset path of avatar `index`'s picker thumbnail, a square face portrait rendered by
/// `tools/render_avatar_thumbnails.py` (lower-cased stem, matching the PNG filenames).
pub fn avatar_thumbnail_path(index: u8) -> String {
    format!("monsters/thumbnails/{}.png", avatar_stem(index).to_lowercase())
}

/// Animation indices in the packs' glTF: identical across all 8 shipped
/// models (verified from the JSON: Bite_Front, Bite_InPlace, Dance, Death,
/// HitRecieve, Idle, Jump, No, Walk, Yes).
const ANIM_IDLE: usize = 5;
const ANIM_WALK: usize = 8;

/// Feet offset: the character origin is the capsule centre; models have their
/// origin at the feet. Half the capsule height (`character.character.ron`
/// `size` 1.5 — kept in lockstep with `insert_character_body`).
const FEET_Y: f32 = -0.75;


pub struct MonsterPlugin;

impl Plugin for MonsterPlugin {
    fn build(&self, app: &mut App) {
        register_gltf_scene_types(app);
        app.insert_resource(LocalMonster(local_monster())).add_systems(
            Update,
            (dress_characters, redress_own_monster, face_own_monster, animate_monsters),
        );
    }
}

/// The world-asset (glTF scene) spawner instantiates through reflection and
/// panics on any scene component missing from the type registry. Bevy's
/// default `reflect_auto_register` feature would register everything (and
/// bloat the wasm with reflection metadata for the whole engine); this build
/// runs `default-features = false`, so register exactly what glTF
/// scene-worlds contain. Named for what it is: anything else that loads a
/// glTF scene needs this too.
fn register_gltf_scene_types(app: &mut App) {
    app.register_type::<Transform>()
            .register_type::<bevy::transform::components::TransformTreeChanged>()
            .register_type::<GlobalTransform>()
            .register_type::<Name>()
            .register_type::<Visibility>()
            .register_type::<InheritedVisibility>()
            .register_type::<ViewVisibility>()
            .register_type::<Children>()
            .register_type::<ChildOf>()
            .register_type::<Mesh3d>()
            .register_type::<MeshMaterial3d<StandardMaterial>>()
            .register_type::<bevy::mesh::skinning::SkinnedMesh>()
            .register_type::<bevy::camera::visibility::DynamicSkinnedMeshBounds>()
            .register_type::<bevy::camera::primitives::Aabb>()
            .register_type::<AnimationPlayer>()
            .register_type::<bevy::animation::AnimationTargetId>()
            .register_type::<bevy::animation::AnimatedBy>()
            .register_type::<bevy::gltf::GltfSceneName>()
            .register_type::<bevy::gltf::GltfMeshName>()
            .register_type::<bevy::gltf::GltfMaterialName>()
            .register_type::<bevy::gltf::GltfExtras>();
}

/// The monster used when no replicated assignment exists (single-player): same
/// hash of the same persistent web resume id the multiplayer server uses, so a
/// given browser is the same monster in both modes. Native has no persistent
/// id (resume id 0) — random per launch.
#[derive(Resource)]
struct LocalMonster(u8);

fn local_monster() -> u8 {
    let id = crate::net::resume_id();
    let id = if id == 0 { rand::random() } else { id };
    monster_index(id)
}

/// Marks a body (own character or remote avatar) as dressed.
#[derive(Component)]
struct MonsterVisual;

/// The monster index currently shown on a dressed body (own or remote). Inserted by
/// `spawn_monster_visual`, so a change to the replicated `NetPlayer::monster` — a player
/// picking a new avatar — can be detected and the visual rebuilt (`redress_own_monster`
/// here for the own avatar, `redress_replicated_players` in `net.rs` for remotes).
#[derive(Component)]
pub struct DisplayedMonster(pub u8);

/// On the *own* body: its yaw pivot entity, rotated from `Yaw` each frame
/// (remote avatars' pivots ride `AvatarVisual` and are rotated by
/// `face_replicated_players` instead — same shape, same sign convention).
#[derive(Component)]
struct OwnMonsterPivot(Entity);

/// On the scene-root entity: which body it dresses and its model path, so the
/// `WorldInstanceReady` observer can wire the animations.
#[derive(Component)]
struct MonsterScene {
    body: Entity,
    path: &'static str,
}

/// On the body once its scene is ready: the `AnimationPlayer` entity plus the
/// graph nodes to switch between.
#[derive(Component)]
struct MonsterAnim {
    player: Entity,
    idle: AnimationNodeIndex,
    walk: AnimationNodeIndex,
    current: AnimationNodeIndex,
}


/// Hang a monster visual under `body`: pivot → scaled scene. Returns the pivot
/// (the caller rotates it to the body's facing). Shared by the single-player /
/// own-avatar dressing below and the remote-avatar path (`draw_replicated_players`).
pub fn spawn_monster_visual(
    commands: &mut Commands,
    body: Entity,
    monster: u8,
    asset_server: &AssetServer,
) -> Entity {
    let (path, scale) = MONSTERS[monster as usize % MONSTERS.len()];
    let scene = commands
        .spawn((
            MonsterScene { body, path },
            WorldAssetRoot(asset_server.load(GltfAssetLabel::Scene(0).from_asset(path))),
            Transform::from_xyz(0.0, FEET_Y, 0.0).with_scale(Vec3::splat(scale)),
        ))
        .observe(setup_monster_animation)
        .id();
    let pivot = commands
        .spawn((Transform::default(), Visibility::default()))
        .add_children(&[scene])
        .id();
    commands
        .entity(body)
        .insert(DisplayedMonster(monster))
        .add_children(&[pivot]);
    pivot
}

/// Rebuild the *own* avatar's visual when the player picks a new one: the server
/// re-replicates `NetPlayer::monster`, which lands on the predicted body (kept synced
/// like `NetName`), so on a mismatch with what's shown, despawn the old pivot (and its
/// glTF instance) and drop the dress markers — `dress_characters` re-dresses next frame
/// from the new index. `MonsterAnim` is overwritten by the new scene's setup, so it
/// isn't cleared here. Remote avatars re-dress the same way via `redress_replicated_players`.
fn redress_own_monster(
    mut commands: Commands,
    changed: Query<
        (Entity, &bad_spaceship_shared::net::NetPlayer, &DisplayedMonster, &OwnMonsterPivot),
        (With<Character>, Changed<bad_spaceship_shared::net::NetPlayer>),
    >,
) {
    for (entity, net, displayed, pivot) in &changed {
        if net.monster == displayed.0 {
            continue;
        }
        commands.entity(pivot.0).despawn();
        commands
            .entity(entity)
            .remove::<(MonsterVisual, OwnMonsterPivot, DisplayedMonster)>();
    }
}

/// Dress the own character (single-player AND the multiplayer predicted
/// avatar — both carry `Character`). In multiplayer the entity also carries
/// the replicated `NetPlayer`, whose server-assigned `monster` wins — that's
/// what everyone else sees, so the mirror must agree.
fn dress_characters(
    mut commands: Commands,
    undressed: Query<
        (Entity, Option<&bad_spaceship_shared::net::NetPlayer>),
        (With<Character>, Without<MonsterVisual>),
    >,
    local: Res<LocalMonster>,
    asset_server: Res<AssetServer>,
) {
    // Ordering note: in multiplayer, `Character` is only ever inserted by
    // `setup_avatar_bodies`, whose query requires `NetPlayer` — so the
    // `unwrap_or(local)` fallback can only fire in genuine single-player.
    for (entity, net) in &undressed {
        let monster = net.map(|n| n.monster).unwrap_or(local.0);
        let pivot = spawn_monster_visual(&mut commands, entity, monster, &asset_server);
        // The body root never had a mesh (the capsule is collider-only), so
        // give it the visibility components the mesh children inherit through.
        commands
            .entity(entity)
            .insert((MonsterVisual, OwnMonsterPivot(pivot), Visibility::default()));
    }
}

/// Turn the own monster to the look yaw (same sign convention as
/// `face_replicated_players`: the models face +Z). The pivot needs no felt-up
/// handling of its own: the character BODY rotates to the felt up (see the felt-up
/// samplers) and the pivot is its child, so the mesh inherits the tilt from the
/// hierarchy like everything else on the character. Compare-before-write so a
/// stationary look doesn't dirty the pivot's transform tree every frame.
fn face_own_monster(
    own: Query<(&Yaw, &OwnMonsterPivot)>,
    mut pivots: Query<&mut Transform>,
) {
    for (yaw, pivot) in &own {
        if let Ok(mut transform) = pivots.get_mut(pivot.0) {
            let rotation = Quat::from_rotation_y(-yaw.0);
            if transform.rotation != rotation {
                transform.rotation = rotation;
            }
        }
    }
}

/// Once the glTF scene instance exists, find its `AnimationPlayer`, give it a
/// graph with the Idle + Walk clips, and start Idle.
fn setup_monster_animation(
    ready: On<WorldInstanceReady>,
    mut commands: Commands,
    scenes: Query<&MonsterScene>,
    children: Query<&Children>,
    mut players: Query<&mut AnimationPlayer>,
    asset_server: Res<AssetServer>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
) {
    let Ok(scene) = scenes.get(ready.entity) else {
        return;
    };
    let path = scene.path;
    for child in children.iter_descendants(ready.entity) {
        let Ok(mut player) = players.get_mut(child) else {
            continue;
        };
        let (graph, nodes) = AnimationGraph::from_clips([
            asset_server.load(GltfAssetLabel::Animation(ANIM_IDLE).from_asset(path)),
            asset_server.load(GltfAssetLabel::Animation(ANIM_WALK).from_asset(path)),
        ]);
        let mut transitions = AnimationTransitions::new();
        transitions.play(&mut player, nodes[0], Duration::ZERO).repeat();
        commands
            .entity(child)
            .insert((AnimationGraphHandle(graphs.add(graph)), transitions));
        commands.entity(scene.body).insert(MonsterAnim {
            player: child,
            idle: nodes[0],
            walk: nodes[1],
            current: nodes[0],
        });
        return;
    }
}

/// Idle ↔ Walk from the player's **input**, never from world motion: a rider
/// standing on a laterally-drifting rocket used to "run in place" because the old
/// implementation measured the body's frame-to-frame world displacement. Walk plays
/// only while a move or jump input is held (looking around doesn't count):
/// - the OWN body reads its local `DirectionalInput` directly (zero delay; written
///   by the single-player input combiner and, in multiplayer, by `apply_net_input`
///   from the same buffered intent the server simulates);
/// - REMOTE avatars read the replicated [`NetMoving`] flag the server mirrors from
///   that player's real input (clients never see other players' inputs).
/// The own predicted body carries both (its round-trip `NetMoving` copy lands on it
/// too); the local input wins.
fn animate_monsters(
    mut bodies: Query<(&mut MonsterAnim, Option<&DirectionalInput>, Option<&NetMoving>)>,
    mut players: Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
) {
    for (mut anim, input, net_moving) in &mut bodies {
        let moving = match (input, net_moving) {
            // x = strafe, z = forward, y = jump intent (see `DirectionalInput`).
            (Some(dir), _) => dir.0 != Vec3::ZERO,
            (None, Some(moving)) => moving.0,
            (None, None) => false,
        };
        let want = if moving { anim.walk } else { anim.idle };
        if want != anim.current {
            if let Ok((mut player, mut transitions)) = players.get_mut(anim.player) {
                transitions
                    .play(&mut player, want, Duration::from_millis(200))
                    .repeat();
                anim.current = want;
            }
        }
    }
}
