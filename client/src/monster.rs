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

use bad_spaceship_shared::{net::monster_index, Character, Yaw};
use bevy::{
    gltf::GltfAssetLabel, prelude::*, world_serialization::WorldAssetRoot,
    world_serialization::WorldInstanceReady,
};

/// (asset path, uniform scale). The scale normalizes each model's rest-pose
/// height (measured from the glTF POSITION bounds) to the capsule's 1.5 m so
/// every monster stands exactly as tall as the body it dresses.
pub const MONSTERS: [(&str, f32); 8] = [
    ("monsters/Alien.glb", 1.5 / 2.06),
    ("monsters/Alien_Tall.glb", 1.5 / 2.11),
    ("monsters/Ghost.glb", 1.5 / 1.40),
    ("monsters/GreenDemon.glb", 1.5 / 1.62),
    ("monsters/Cyclops.glb", 1.5 / 1.67),
    ("monsters/Demon.glb", 1.5 / 1.62),
    ("monsters/Yeti.glb", 1.5 / 1.67),
    ("monsters/Mushroom.glb", 1.5 / 2.07),
];

/// Animation indices in the packs' glTF: identical across all 8 shipped
/// models (verified from the JSON: Bite_Front, Bite_InPlace, Dance, Death,
/// HitRecieve, Idle, Jump, No, Walk, Yes).
const ANIM_IDLE: usize = 5;
const ANIM_WALK: usize = 8;

/// Feet offset: the character origin is the capsule centre; models have their
/// origin at the feet. Half the capsule height (`character.character.ron`
/// `size` 1.5 — kept in lockstep with `insert_character_body`).
const FEET_Y: f32 = -0.75;

/// Walking faster than this plays Walk; slower plays Idle. One threshold (no
/// hysteresis) is fine because the character's speed is far from it in both
/// states (0 idle vs ~13 max_speed).
const WALK_SPEED: f32 = 1.0;

pub struct MonsterPlugin;

impl Plugin for MonsterPlugin {
    fn build(&self, app: &mut App) {
        // The world-asset (glTF scene) spawner instantiates through reflection
        // and panics on any scene component missing from the type registry.
        // Bevy's default `reflect_auto_register` feature would register
        // everything (and bloat the wasm with reflection metadata for the
        // whole engine); this build runs `default-features = false`, so
        // register exactly what the monster GLBs contain.
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
        app.insert_resource(LocalMonster(local_monster()))
            .add_systems(Update, (dress_characters, face_own_monster, animate_monsters));
    }
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
pub struct MonsterVisual;

/// The yaw pivot on the *own* body, rotated from `Yaw` each frame (remote
/// avatars' pivots are rotated by `face_replicated_players` instead).
#[derive(Component)]
struct OwnMonsterPivot;

/// On the scene-root entity: which body/monster it dresses, so the
/// `WorldInstanceReady` observer can wire the animations.
#[derive(Component)]
struct MonsterScene {
    body: Entity,
    monster: u8,
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

/// Previous frame's position, for animation speed (works identically for the
/// locally-simulated own body and interpolated remote avatars).
#[derive(Component)]
struct LastPos(Vec3);

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
            MonsterScene { body, monster },
            WorldAssetRoot(asset_server.load(GltfAssetLabel::Scene(0).from_asset(path))),
            Transform::from_xyz(0.0, FEET_Y, 0.0).with_scale(Vec3::splat(scale)),
        ))
        .observe(setup_monster_animation)
        .id();
    let pivot = commands
        .spawn((Transform::default(), Visibility::default()))
        .add_children(&[scene])
        .id();
    commands.entity(body).add_children(&[pivot]);
    pivot
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
    for (entity, net) in &undressed {
        let monster = net.map(|n| n.monster).unwrap_or(local.0);
        let pivot = spawn_monster_visual(&mut commands, entity, monster, &asset_server);
        commands.entity(pivot).insert(OwnMonsterPivot);
        // The body root never had a mesh (the capsule is collider-only), so
        // give it the visibility components the mesh children inherit through.
        commands
            .entity(entity)
            .insert((MonsterVisual, Visibility::default()));
    }
}

/// Turn the own monster to the look yaw (same sign convention as
/// `face_replicated_players`: the models face +Z).
fn face_own_monster(
    own: Query<(&Yaw, &Children), With<MonsterVisual>>,
    mut pivots: Query<&mut Transform, With<OwnMonsterPivot>>,
) {
    for (yaw, children) in &own {
        for child in children.iter() {
            if let Ok(mut transform) = pivots.get_mut(child) {
                transform.rotation = Quat::from_rotation_y(-yaw.0);
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
    let (path, _) = MONSTERS[scene.monster as usize % MONSTERS.len()];
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

/// Idle ↔ Walk from the body's horizontal speed, measured as the frame-to-frame
/// `GlobalTransform` delta — one code path that works for the locally-simulated
/// own body, the predicted avatar, and interpolated remote avatars alike.
fn animate_monsters(
    time: Res<Time>,
    mut commands: Commands,
    mut bodies: Query<(Entity, &GlobalTransform, &mut MonsterAnim, Option<&mut LastPos>)>,
    mut players: Query<(&mut AnimationPlayer, &mut AnimationTransitions)>,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    for (entity, global, mut anim, last) in &mut bodies {
        let pos = global.translation();
        let Some(mut last) = last else {
            commands.entity(entity).insert(LastPos(pos));
            continue;
        };
        let speed = ((pos - last.0) * Vec3::new(1.0, 0.0, 1.0)).length() / dt;
        last.0 = pos;
        let want = if speed > WALK_SPEED { anim.walk } else { anim.idle };
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
