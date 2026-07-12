use avian3d::prelude::{
    Collider, CollisionLayers, Collisions, LinearVelocity, LockedAxes, Mass, Position, RigidBody,
    SphericalJoint,
};
use bevy::prelude::*;
use bevy::reflect::TypePath;
use rand::Rng;

use serde::Deserialize;

use crate::{
    Character, DirectionalInput, GameStickDirectionalInput, KeyboardDirectionalInput, Player, Yaw,
};

pub struct CharacterPlugin;

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut App) {
        // Input *merging* stays in `Update`: it's sampled once per render frame
        // from the device input written there (and `combine` zeroes the keyboard
        // accumulator, which is only safe once per frame). The velocity-applying
        // systems move to `FixedUpdate` so movement advances exactly once per
        // simulation tick. This is a prerequisite for the upcoming client-side
        // prediction + rollback (which replays per-tick inputs deterministically),
        // and it removes the old frame-rate dependence — movement used to run in
        // `Update`, so a 120 Hz display literally moved the character faster.
        app.add_systems(
            Update,
            (
                combine_directional_inputs,
                // One-shot: copy the RON `max_speed`/`jump_force` into `MovementTuning`
                // once the config asset loads, so the panel starts at the RON values.
                seed_movement_tuning,
                // Suppressed in multiplayer — the predicted networked avatar (client)
                // / the per-client `ServerAvatar` (server) is the character instead.
                spawn.run_if(not(resource_exists::<crate::SuppressLocalPlayer>)),
                build_server_avatar,
            ),
        )
            .add_systems(
                FixedUpdate,
                (
                    touching_ground,
                    walk_based_on_input.after(touching_ground),
                    // walk + jump both write `LinearVelocity` (different axes, but
                    // the same component), so order them explicitly for a
                    // deterministic result under rollback replay.
                    jump_based_on_input
                        .after(touching_ground)
                        .after(walk_based_on_input),
                )
                    .in_set(CharacterMovement),
            )
            // Run the fixed timestep at the netcode tick rate (60 Hz) so single-
            // player physics + movement match the multiplayer simulation tick
            // (Bevy's default `Time<Fixed>` is 64 Hz). Avian steps on this too.
            .insert_resource(Time::<Fixed>::from_duration(crate::net::TICK))
            .init_resource::<MovementTuning>()
            .init_asset::<Config>();
    }
}

/// Roll the character's ground-contact state back with the physics state. Without
/// this, a rollback restores the avatar's Position/velocity to the confirmed tick
/// but replays with the CURRENT tick's support state — a stale
/// `GroundVelocity`/`TouchingGround`/`LastSupport` for the first replayed movement
/// ticks, one more systematic source of replay divergence (and so of visible
/// correction wobble while walking).
///
/// Called by the multiplayer client AFTER lightyear's `ClientPlugins`: the
/// registration silently no-ops unless the `PredictionRegistry` resource already
/// exists (it does NOT defer), so registering from `CharacterPlugin::build` —
/// which runs before the netcode plugins — would do nothing.
pub fn register_ground_state_rollback(app: &mut App) {
    use lightyear::prelude::PredictionAppRegistrationExt;
    app.local_rollback::<TouchingGround>();
    app.local_rollback::<GroundVelocity>();
    app.local_rollback::<LastSupport>();
}

/// Selectable horizontal-movement model, chosen live from the in-game Movement
/// panel (`client::ui::show_movement_panel`). Each is a different way of steering the
/// character's horizontal velocity toward the input direction; they exist so the
/// "feel" can be A/B tested and fine-tuned at runtime. Read by `walk_based_on_input`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MovementModel {
    /// Frame-rate-independent exponential approach toward the target velocity (a
    /// smoothed lerp) — closest to the game's original feel, snappier as
    /// `smooth_rate` rises. Soft, symmetric momentum.
    #[default]
    Smooth,
    /// Velocity snaps straight to the target on the ground (zero momentum, maximum
    /// snap); in the air it steers by `air_control`. Arcade feel.
    Instant,
    /// Linear acceleration toward the target with a separate (usually higher)
    /// deceleration back to rest — crisp, distinct start/stop (platformer feel).
    Accel,
    /// Quake/Source-style: ground friction, then accelerate toward the wish direction
    /// up to the target speed. Momentum + air-strafing — the classic "snappy but
    /// slidey" FPS feel.
    Source,
}

impl MovementModel {
    /// All models, in panel/combo-box order.
    pub const ALL: [MovementModel; 4] = [
        MovementModel::Smooth,
        MovementModel::Instant,
        MovementModel::Accel,
        MovementModel::Source,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MovementModel::Smooth => "Smooth (exponential)",
            MovementModel::Instant => "Instant (arcade)",
            MovementModel::Accel => "Accel (platformer)",
            MovementModel::Source => "Source (FPS)",
        }
    }
}

/// Live, runtime-tunable movement parameters, edited from the in-game Movement panel
/// and read by the `FixedUpdate` movement systems. One resource carries the superset
/// of every model's knobs; each model uses only the subset relevant to it (the panel
/// reveals just those). `max_speed`/`jump_force` are seeded once from the character
/// `Config` asset (`seed_movement_tuning`) so they start at the RON values.
#[derive(Resource, Clone, Debug)]
pub struct MovementTuning {
    pub model: MovementModel,
    /// Set once `seed_movement_tuning` has copied the config defaults in.
    seeded: bool,

    // Shared
    /// Target ground speed (m/s) at full input.
    pub max_speed: f32,
    /// Fraction (0..1) of ground responsiveness retained in the air.
    pub air_control: f32,

    // Smooth
    /// Exponential approach rate (1/s). Higher = snappier.
    pub smooth_rate: f32,

    // Accel
    /// Acceleration toward the target (m/s²).
    pub accel: f32,
    /// Deceleration back to rest when there is no input (m/s²).
    pub decel: f32,

    // Source
    /// Ground friction (1/s).
    pub friction: f32,
    /// Ground acceleration coefficient.
    pub ground_accel: f32,
    /// Air acceleration coefficient.
    pub air_accel: f32,
    /// Speed floor for friction so slow motion still stops crisply (m/s).
    pub stop_speed: f32,

    // Jump (shared)
    /// Upward launch speed on jump (m/s).
    pub jump_force: f32,
    /// Extra downward acceleration while descending (m/s²); 0 = off. A snappier, less
    /// floaty fall without touching global gravity (so the parts are unaffected).
    pub fall_multiplier: f32,
}

impl Default for MovementTuning {
    fn default() -> Self {
        Self {
            // Locked-in defaults (2026-07-04): the Accel/platformer model with the
            // knobs Sam dialed in. Because the *server* inits this same resource, the
            // authoritative sim now matches the client's prediction by default (no
            // reconciliation bounce), whereas a non-default model picked in the panel
            // would still diverge from the server — the multiplayer caveat.
            // `max_speed`/`jump_force` mirror `character.character.ron`, which the
            // one-shot `seed_movement_tuning` copies over these on config load.
            model: MovementModel::Accel,
            seeded: false,
            max_speed: 11.0,
            air_control: 0.25,
            smooth_rate: 14.0,
            accel: 140.0,
            decel: 170.0,
            friction: 8.0,
            ground_accel: 14.0,
            air_accel: 2.0,
            stop_speed: 1.5,
            jump_force: 7.5,
            fall_multiplier: 0.0,
        }
    }
}

impl MovementTuning {
    /// A copy-pasteable summary of the current model + the knobs it actually uses
    /// (plus the shared jump values), for the panel's "Copy settings" button.
    pub fn settings_string(&self) -> String {
        let mut s = format!(
            "model: {}\nmax_speed: {:.2}\n",
            self.model.label(),
            self.max_speed
        );
        match self.model {
            MovementModel::Smooth => {
                s += &format!(
                    "smooth_rate: {:.2}\nair_control: {:.2}\n",
                    self.smooth_rate, self.air_control
                );
            }
            MovementModel::Instant => {
                s += &format!("air_control: {:.2}\n", self.air_control);
            }
            MovementModel::Accel => {
                s += &format!(
                    "accel: {:.1}\ndecel: {:.1}\nair_control: {:.2}\n",
                    self.accel, self.decel, self.air_control
                );
            }
            MovementModel::Source => {
                s += &format!(
                    "friction: {:.2}\nground_accel: {:.2}\nair_accel: {:.2}\nstop_speed: {:.2}\n",
                    self.friction, self.ground_accel, self.air_accel, self.stop_speed
                );
            }
        }
        s += &format!(
            "jump_force: {:.2}\nfall_multiplier: {:.1}\n",
            self.jump_force, self.fall_multiplier
        );
        s
    }
}

/// Copy the character `Config`'s `max_speed`/`jump_force` into `MovementTuning` once,
/// the first frame the config asset is available — so the panel's initial values match
/// the RON rather than the hard-coded `Default`. The `seeded` latch makes it a one-shot;
/// afterward the user's live edits stand.
fn seed_movement_tuning(mut tuning: ResMut<MovementTuning>, configs: Res<Assets<Config>>) {
    if tuning.seeded {
        return;
    }
    if let Some((_, config)) = configs.iter().next() {
        tuning.max_speed = config.max_speed;
        tuning.jump_force = config.jump_force;
        tuning.seeded = true;
    }
}

// Bevy 0.12's asset rework replaced `TypeUuid` with the `Asset` derive
// (which still requires `TypePath`); the type id is derived, not a manual UUID.
#[derive(Asset, Deserialize, Clone, TypePath, Debug)]
pub struct Config {
    size: f32,
    max_speed: f32,
    jump_force: f32,
}

impl Config {
    /// The character's total height (capsule, round top and bottom), in metres. Exposed so the client's
    /// predicted-avatar setup can build the same body the single-player `spawn`
    /// does, from the loaded config asset.
    pub fn size(&self) -> f32 {
        self.size
    }
}

#[derive(Default, Bundle)]
struct CharacterBundle {
    character: Character,
    // Avian splits rapier's single `Velocity` into separate `LinearVelocity` /
    // `AngularVelocity`; the character only reads/writes linear velocity.
    linear_velocity: LinearVelocity,
    touching_ground: TouchingGround,
    ground_velocity: GroundVelocity,
    last_support: LastSupport,
}

/// Insert the core character physics body onto an entity. Shared by the
/// single-player `spawn`, the server's `build_server_avatar`, and the client's
/// predicted-avatar setup. Does NOT set `Transform`/`Position` — the caller sets
/// the spawn pose (single-player/server) or replication provides it (client
/// predicted). The capsule collider, rotation lock, unit mass, and the
/// movement-input component (`DirectionalInput`) plus `Character`/velocity/
/// ground-contact (`CharacterBundle`) match what every controllable character needs.
pub fn insert_character_body(entity: &mut EntityCommands, size: f32) {
    insert_remote_avatar_body(entity, size);
    entity.insert((CharacterBundle::default(), DirectionalInput::default()));
}

/// The rider capsule every avatar body is built from — the OWNER's controllable
/// character ([`insert_character_body`] layers the movement-input/ground-contact
/// bundle on top) and, on a client, every *remote* player's predicted avatar, which
/// gets exactly this and nothing more. One definition so the client-predicted remote
/// body can never drift from the body the server simulates (a collider/mass mismatch
/// would surface as permanent rollback churn on every remote avatar).
///
/// The client predicts every avatar (`PredictionTarget::All`), so an *other* player's
/// avatar must be a real dynamic body that rests on the predicted deck via contact —
/// that's what makes it ride a climbing rocket on the same timeline as the deck
/// instead of interpolating a frame behind and sinking through it. It carries no
/// `Character`, so the single-player movement systems and the `Character`-keyed
/// monster dresser leave it alone (it's driven only by physics + rollback against the
/// server — its replicated `LinearVelocity` lets the input-less body coast at the
/// player's real velocity between snapshots — and dressed/faced by the remote-avatar
/// path).
///
/// Body details:
/// - Pill body: a vertical capsule (round top and bottom) with the same TOTAL height
///   as the old `size`-diameter sphere — radius size/3 plus a size/3 cylindrical
///   middle = size tall, so the collider centre sits at the same height above ground
///   contact and the camera/hold-point geometry tuned for the sphere carries over;
///   the body just slims from `size` wide to (2/3)·size. Rotation stays locked, so
///   it never tips. (Avian's `capsule` takes the radius and the cylindrical
///   mid-section length, not the total height.)
/// - NO `SweptCcd`, deliberately (the parts carry it; the character must not):
///   sweeping a body in *persistent contact* with a fast co-moving platform (riding a
///   rocket at 100+ m/s) occasionally clamps it to a stale TOI while the platform
///   advances a full ~2 m tick-step, manufacturing a huge synthetic overlap —
///   measured as a +234 m/s ejection one tick after a perfectly soft landing. Without
///   CCD, jump landings can penetrate a few cm and very rarely buck the rider
///   (~+20 m/s once per many hops); with it, even standing riders get launched. The
///   lesser evil, verified both ways with the recorder.
/// - Mass pinned to 1.0; movement sets velocity directly so this only scales how the
///   avatar shoves parts on contact.
pub fn insert_remote_avatar_body(entity: &mut EntityCommands, size: f32) {
    entity.insert((
        RigidBody::Dynamic,
        LockedAxes::ROTATION_LOCKED,
        Collider::capsule(size / 3.0, size / 3.0),
        Mass(1.0),
    ));
}

fn spawn(
    mut commands: Commands,
    players_without_characters: Query<Entity, (With<Player>, Without<Character>)>,
    configs: Res<Assets<Config>>,
) {
    if let Some((_, config)) = configs.iter().next() {
        for player_entity in players_without_characters.iter() {
            let mut entity = commands.entity(player_entity);
            insert_character_body(&mut entity, config.size);
            // Bevy 0.15: bare `Transform` (it now requires `GlobalTransform`).
            entity.insert(Transform::from_xyz(0.0, 10.0, 0.0));
        }
    }
}

/// System set wrapping the `FixedUpdate` movement systems, so the server's
/// input-bridge (which writes `DirectionalInput`/`Yaw` from the networked input)
/// can be ordered `.before` them.
#[derive(SystemSet, Clone, Hash, Debug, PartialEq, Eq)]
pub struct CharacterMovement;

/// Marks a server-side networked avatar that needs its character body assembled —
/// the multiplayer equivalent of a local `Player`. The server adds this to each
/// client's replicated avatar; `build_server_avatar` then gives it the same Avian
/// body the single-player `spawn` builds, so the server simulates it authoritatively
/// from the client's input intent.
#[derive(Default, Component)]
pub struct ServerAvatar;

/// An optional spawn position for a `ServerAvatar`, honored by `build_server_avatar`
/// instead of a random spawn point. The server sets it for a *reconnecting* client
/// (the position resolved at connect from the resume id in the connect token), so the
/// avatar is assembled directly at its remembered spot — its first replicated
/// `Position` is the saved one, with no easing on the client.
#[derive(Component, Clone, Copy)]
pub struct InitialPose(pub Vec3);

/// Radius (m) of the disc around the platform centre a *fresh* avatar spawns into.
/// Fresh avatars must NOT all land on the exact origin: two `ROTATION_LOCKED`,
/// equal-mass spheres at zero separation share one contact point with no defined
/// separation normal, so the solver oscillates them every tick (and the existing
/// player's predicted body snaps onto the pile) — the "a new joiner corrupts the
/// first player" glitch. Any non-zero offset gives the contact a stable normal; a
/// few metres of spread (well inside the 50 m platform) keeps joiners comfortably
/// apart while staying on the platform.
const SPAWN_SPREAD_RADIUS: f32 = 8.0;

/// A fresh spawn position: a random point on the spawn disc at ground level (NOT the
/// shared origin — two avatars there overlap exactly and the solver explodes). Used
/// for a fresh avatar's initial pose (`build_server_avatar`) and to teleport an
/// avatar back on request (the server's "reset to spawn"), so both land on the same
/// valid on-platform spot rule.
pub fn spawn_position() -> Vec3 {
    let mut rng = rand::thread_rng();
    let angle = rng.gen_range(0.0..std::f32::consts::TAU);
    let radius = rng.gen_range(2.0..SPAWN_SPREAD_RADIUS);
    Vec3::new(radius * angle.cos(), 0.0, radius * angle.sin())
}

/// Assemble the character body for each `ServerAvatar` that doesn't have one yet,
/// once the character `Config` (its size) is loaded. Mirrors `spawn`, but driven by
/// the networked marker and seeded with the movement-input component the server's
/// bridge writes (`DirectionalInput`) plus `Yaw` (the bridge writes it from the
/// client's look angle each tick).
fn build_server_avatar(
    mut commands: Commands,
    avatars: Query<(Entity, Option<&InitialPose>), (With<ServerAvatar>, Without<Character>)>,
    configs: Res<Assets<Config>>,
) {
    if let Some((_, config)) = configs.iter().next() {
        for (entity, initial) in avatars.iter() {
            // A reconnecting client's avatar is built directly at its remembered
            // position (`InitialPose`, resolved at connect from the resume id), so its
            // first replicated pose is the saved one — no easing. A fresh client spawns
            // at a random point on the spawn disc (NOT the shared origin — two avatars
            // there overlap exactly and the solver explodes; see `SPAWN_SPREAD_RADIUS`),
            // at ground level (a tiny settle, NOT the single-player y=10 drop-in, which a
            // predicting client would mispredict in slow motion at the chaotic connect
            // moment). Seed both Transform and Position because Avian's transform-sync is
            // disabled in multiplayer.
            let pos = initial.map(|p| p.0).unwrap_or_else(spawn_position);
            let mut e = commands.entity(entity);
            insert_character_body(&mut e, config.size);
            e.insert((
                Transform::from_translation(pos),
                Position(pos),
                Yaw::default(),
                // Until `assign_rooms` scopes this avatar to its room's collision layer
                // (on the first input), collide with the GROUND ONLY — never with parts
                // or avatars in any room. A fresh avatar has no room yet; without this it
                // takes Avian's default "membership bit 0 / filter ALL" and, for the ~1
                // RTT before assignment, shoves every room's blocks and any existing
                // avatar it spawns near (cross-room interference + part of the new-joiner
                // glitch). `assign_rooms` swaps in the real per-room layer on first input.
                CollisionLayers::from_bits(crate::map::GROUND_LAYER, crate::map::GROUND_LAYER),
            ));
            e.remove::<InitialPose>();
        }
    }
}

fn combine_directional_inputs(
    mut query: Query<(
        &mut KeyboardDirectionalInput,
        &GameStickDirectionalInput,
        &mut DirectionalInput,
    )>,
) {
    for (mut keyboard_directional_input, gamepad_directional_input, mut directional_input) in
        query.iter_mut()
    {
        directional_input.0 = Vec3::ZERO;
        directional_input.0.x = keyboard_directional_input.0.x + gamepad_directional_input.0.x;
        directional_input.0.y = keyboard_directional_input.0.y + gamepad_directional_input.0.y;
        directional_input.0.z = keyboard_directional_input.0.z + gamepad_directional_input.0.z;
        // Clamp (not normalize) so analog inputs keep their sub-unit magnitude for
        // variable speed — the touch joystick's response curve relies on this.
        // Keyboard and gamepad both arrive at unit length already, so capping the
        // max leaves them (and diagonal WASD) exactly as before.
        directional_input.0 = directional_input.0.clamp_length_max(1.0);

        // Now that we've read this, reset it so it can be summed up again next frame
        keyboard_directional_input.0 = Vec3::ZERO;
    }
}

/// Step `from` toward `to` by at most `max_delta`, snapping to `to` once within reach
/// (the `Accel` model's constant-acceleration integrator).
fn move_toward(from: Vec3, to: Vec3, max_delta: f32) -> Vec3 {
    let delta = to - from;
    let dist = delta.length();
    if dist <= max_delta || dist <= 1e-6 {
        to
    } else {
        from + delta / dist * max_delta
    }
}

#[derive(Default, Component, Clone, PartialEq, Debug)]
struct TouchingGround(bool);

/// The velocity of whatever the character is standing on (zero when airborne or
/// on static ground). Movement and jumping compose with it so the character
/// works on *moving* supports: standing on an ascending rocket platform
/// (~120 m/s up), a world-frame jump (`vy = jump_force`) instantly flung the
/// rider off — the platform out-accelerated them within a tick.
#[derive(Default, Component, Clone, PartialEq, Debug)]
struct GroundVelocity(Vec3);

/// The support's velocity at the character's last grounded tick, plus how many
/// ticks ago that was. Fed by `touching_ground`, consumed by the separation
/// clamp in `jump_based_on_input`: a body that *just* left the ground can't
/// legitimately move faster relative to its support than a jump (vertically)
/// or a sprint (horizontally) — anything above that is a contact-solver kick
/// (a deep landing or an edge contact on a fast platform occasionally ejects
/// at +15…+200 m/s relative, vertically or diagonally) and gets clamped,
/// turning a violent buck into motion the deck can catch.
#[derive(Default, Component, Clone, PartialEq, Debug)]
struct LastSupport {
    velocity: Vec3,
    ticks_since_grounded: u32,
}

fn touching_ground(
    mut query: Query<(Entity, &mut TouchingGround, &mut GroundVelocity, &mut LastSupport)>,
    // Avian's `Collisions` system param yields only the touching contact pairs
    // (a convenience view over the `ContactGraph`); `collisions_with` is the
    // rapier `contact_pairs_with` equivalent.
    collisions: Collisions,
    velocities: Query<&LinearVelocity>,
) {
    for (entity, mut touching_ground, mut ground_velocity, mut last_support) in query.iter_mut() {
        touching_ground.0 = false;
        ground_velocity.0 = Vec3::ZERO;
        for pair in collisions.collisions_with(entity) {
            if let Some(contact) = pair.find_deepest_contact() {
                // Avian's `penetration` is positive when the bodies overlap — the
                // opposite sign from rapier's `dist()`. The old `dist < 0.002`
                // "in contact" test becomes `penetration > -0.002`.
                if contact.penetration > -0.002 {
                    touching_ground.0 = true;
                    // The support's velocity (static ground has none → zero).
                    let other = if pair.collider1 == entity {
                        pair.collider2
                    } else {
                        pair.collider1
                    };
                    ground_velocity.0 =
                        velocities.get(other).map(|v| v.0).unwrap_or(Vec3::ZERO);
                    break;
                }
            }
        }
        // Remember the support's velocity across the moment of separation
        // (see `LastSupport`); while airborne, only the age advances.
        if touching_ground.0 {
            last_support.velocity = ground_velocity.0;
            last_support.ticks_since_grounded = 0;
        } else {
            last_support.ticks_since_grounded = last_support.ticks_since_grounded.saturating_add(1);
        }
    }
}

fn walk_based_on_input(
    time: Res<Time>,
    mut query: Query<(
        Entity,
        &DirectionalInput,
        &Yaw,
        &mut LinearVelocity,
        &TouchingGround,
        &GroundVelocity,
    )>,
    lock_joints: Query<&SphericalJoint, With<crate::part::LockJoint>>,
    tuning: Res<MovementTuning>,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    // A body pinned by a player-lock weld is frozen to its support: the joint owns its
    // motion, and the walk model's every-tick velocity write (even at zero input it
    // drives horizontal velocity toward the support's) would fight the constraint.
    let locked = crate::part::locked_bodies(&lock_joints);
    for (entity, directional_input, yaw, mut velocity, touching_ground, ground_velocity) in
        query.iter_mut()
    {
        if locked.contains(&entity) {
            continue;
        }
        let grounded = touching_ground.0;
        // The body is ROTATION_LOCKED (its rotation is owned by physics), so the move
        // basis comes from the look `Yaw`, not the body transform: `back()` = +Z ("W"),
        // `left()` = -X ("A"), both yawed by `-yaw` (see `mouse_motion`). `wish`'s
        // magnitude (≤ 1 for analog sticks) doubles as the throttle.
        let look = Quat::from_rotation_y(-yaw.0);
        let wish =
            look * Vec3::Z * directional_input.0.z + look * Vec3::NEG_X * directional_input.0.x;
        // Walk *relative to the support*: on a moving platform the target speed is
        // measured against the platform, not the world, so standing still on a
        // drifting rocket doesn't read as "moving" (and get braked against it).
        // `GroundVelocity` is zero whenever airborne or on static ground
        // (`touching_ground` clears it), so those behave exactly as before.
        let support = Vec3::new(ground_velocity.0.x, 0.0, ground_velocity.0.z);
        let horizontal = Vec3::new(velocity.0.x, 0.0, velocity.0.z) - support;

        let new_horizontal = match tuning.model {
            MovementModel::Smooth => {
                // Exponential approach to the target (frame-rate independent). When the
                // target is zero this decays to rest, so it also handles stopping.
                let target = wish * tuning.max_speed;
                let mut rate = tuning.smooth_rate;
                if !grounded {
                    rate *= tuning.air_control;
                }
                // libm: identical across wasm/native (prediction determinism).
                let alpha = 1.0 - libm::expf(-rate * dt);
                horizontal.lerp(target, alpha)
            }
            MovementModel::Instant => {
                // Snap to the target on the ground; lightly steer in the air.
                let target = wish * tuning.max_speed;
                if grounded {
                    target
                } else {
                    horizontal.lerp(target, tuning.air_control)
                }
            }
            MovementModel::Accel => {
                // Constant acceleration toward the target; a separate (usually larger)
                // deceleration when there's no input.
                let target = wish * tuning.max_speed;
                let has_input = wish.length_squared() > 1e-6;
                let mut rate = if has_input { tuning.accel } else { tuning.decel };
                if !grounded {
                    rate *= tuning.air_control;
                }
                move_toward(horizontal, target, rate * dt)
            }
            MovementModel::Source => {
                // Quake/Source: friction on the ground, then accelerate toward the wish
                // direction up to the target speed (air acceleration enables strafing).
                let mut h = horizontal;
                if grounded {
                    let speed = h.length();
                    if speed > 0.0 {
                        let control = speed.max(tuning.stop_speed);
                        let drop = control * tuning.friction * dt;
                        h *= (speed - drop).max(0.0) / speed;
                    }
                }
                let wish_speed = wish.length() * tuning.max_speed;
                if wish_speed > 0.0 {
                    let wish_dir = wish.normalize();
                    let add_speed = wish_speed - h.dot(wish_dir);
                    if add_speed > 0.0 {
                        let coef = if grounded {
                            tuning.ground_accel
                        } else {
                            tuning.air_accel
                        };
                        let accel_speed = (coef * dt * wish_speed).min(add_speed);
                        h += wish_dir * accel_speed;
                    }
                }
                h
            }
        };

        // Movement owns only the horizontal plane; gravity/jump own the vertical axis.
        velocity.0.x = support.x + new_horizontal.x;
        velocity.0.z = support.z + new_horizontal.z;
    }
}

fn jump_based_on_input(
    time: Res<Time>,
    mut query: Query<(
        Entity,
        &DirectionalInput,
        &mut LinearVelocity,
        &TouchingGround,
        &GroundVelocity,
        &LastSupport,
    )>,
    lock_joints: Query<&SphericalJoint, With<crate::part::LockJoint>>,
    tuning: Res<MovementTuning>,
) {
    let dt = time.delta_secs();
    // Locked riders are welded in place — jumps and the separation clamps are the
    // weld's business now (see `walk_based_on_input`).
    let locked = crate::part::locked_bodies(&lock_joints);
    for (entity, directional_input, mut velocity, touching_ground, ground_velocity, last_support) in
        query.iter_mut()
    {
        if locked.contains(&entity) {
            continue;
        }
        // Separation clamp (see `LastSupport`): freshly airborne and moving
        // faster *relative to the support* than the player could have earned
        // (jump vertically, sprint horizontally) ⇒ a contact-solver kick — a
        // deep landing or an edge contact on a fast platform, kicking upward or
        // diagonally — not real motion. Clamp each axis group to its earnable
        // limit. The vertical margin is deliberately TINY: the support's
        // recorded velocity jitters ±2 m/s tick-to-tick under thrust, and a
        // 2 m/s margin let ~+11 m/s bounces sail under a jitter-high ceiling
        // (every wide-deck landing bucked the rider). A spuriously-triggered
        // clamp is harmless — it sets exactly a legitimate jump velocity — so
        // err toward clamping.
        // Diagnostics (BS_DEBUG_CLAMP): print clamp decisions for fast-relative
        // states near the ground — how the kick-vs-clamp forensics in this file
        // were gathered. Checked once (hot path).
        static DEBUG_CLAMP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let debug_clamp = *DEBUG_CLAMP.get_or_init(|| std::env::var("BS_DEBUG_CLAMP").is_ok());
        if debug_clamp && last_support.ticks_since_grounded <= 4 {
            let rel = velocity.0 - last_support.velocity;
            if rel.length() > 5.0 {
                println!(
                    "[clamp?] touching={} since={} rel=({:.1},{:.1},{:.1}) support_vy={:.1}",
                    touching_ground.0,
                    last_support.ticks_since_grounded,
                    rel.x, rel.y, rel.z,
                    last_support.velocity.y
                );
            }
        }
        if !touching_ground.0 && last_support.ticks_since_grounded <= 2 {
            let rel = velocity.0 - last_support.velocity;
            if rel.y > tuning.jump_force + 0.25 {
                // Grant a snap-down (+1 relative — re-lands within a couple of
                // ticks), NOT a jump: kicks scale with platform speed (+49 m/s
                // measured at 46 m/s ascent), and converting each one into an
                // involuntary full hop stacked ~0.9 s of extra airborne drift
                // onto the rider — which is how they still ended up off the
                // deck edge. A real jump (rel exactly jump_force) passes.
                if debug_clamp {
                    println!("[clamp!] vy {:.1} -> {:.1}", velocity.0.y, last_support.velocity.y + 1.0);
                }
                velocity.0.y = last_support.velocity.y + 1.0;
            }
            // Horizontally, keep the flat `max_speed + 3` cap. (An earned-speed cap —
            // "what the walk model commanded on the last grounded tick + margin" — was
            // tried against the 8–11 m/s diagonal kicks seen on thrust-vectoring
            // stacks, and made things WORSE: a stand-still rider's earned speed is ~0,
            // so every micro-bounce on a swaying deck velocity-yanked the rider to
            // match it, pumping momentum into the light deck through the next contact
            // until the stack spun apart — recorder showed |ω| → 173 rad/s with a
            // rider standing dead still. The diagonal drift that remains under this
            // flat cap is real airborne motion over a swaying deck, not a kick.)
            let horizontal = Vec3::new(rel.x, 0.0, rel.z);
            let cap = tuning.max_speed + 3.0;
            if horizontal.length() > cap {
                let scaled = horizontal * (cap / horizontal.length());
                velocity.0.x = last_support.velocity.x + scaled.x;
                velocity.0.z = last_support.velocity.z + scaled.z;
            }
        }
        // Jump: while grounded and the up-intent is held, set the upward speed directly
        // (the body's up is always +Y — it's rotation-locked). Held-space re-jumps each
        // tick it's grounded, matching the original behaviour. The jump is *relative to
        // the support*: a world-frame `vy = jump_force` on a platform ascending at
        // 120 m/s instantly clamped the rider ~112 m/s slower than their ride.
        if directional_input.0.y > 0.0 && touching_ground.0 {
            velocity.0.y = ground_velocity.0.y + tuning.jump_force;
        }
        // Snappier, less-floaty fall: extra downward acceleration while descending,
        // applied only to the character so global gravity (and the parts) is untouched.
        if !touching_ground.0 && velocity.0.y < 0.0 && tuning.fall_multiplier > 0.0 {
            velocity.0.y -= tuning.fall_multiplier * dt;
        }
    }
}
