//! Rocket launch sequence — the client half (UI + thrust), single-player *and*
//! multiplayer.
//!
//! When the character is touching its room's **main assembly** (the largest group of
//! parts jointed together — the thrust arrow / COM-orb set), a "slide to launch" control
//! appears at the top-centre. Completing the swipe starts a `3 → 2 → 1 → Blastoff!`
//! countdown; at blastoff every joint pinning the assembly to the ground is cut and the
//! assembly's rockets fire with balanced, anti-spin thrust (see
//! [`bad_spaceship_shared::launch`]).
//!
//! **Two modes, one feel:**
//! - *Single-player* is client-authoritative: this file owns the countdown, cuts the
//!   ground joints, and applies thrust to the local sim.
//! - *Multiplayer* is server-authoritative: the swipe sends a [`RequestLaunch`], the
//!   server runs the countdown + cuts ground joints, and replicates the state on the
//!   room's orb ([`NetLaunch`]). The countdown banner is drawn from that replicated
//!   state, and the same balanced thrust is applied here to the **predicted** rockets so
//!   the liftoff is smooth rather than rollback-jittered (the server applies the identical
//!   force, so prediction converges).

use avian3d::prelude::{
    AngularVelocity, ComputedMass, Forces, Gravity, Position, Rotation, SphericalJoint,
    WriteRigidBodyForces,
};
use bad_spaceship_shared::launch::{
    balanced_assembly_thrust, full_rocket_thrust, gimbal_step, gimbaled_rocket_thrust,
    measure_assembly_spin, AssemblySpin, LAUNCH_COUNTDOWN_SECS,
};
use bad_spaceship_shared::net::{ControlChannel, InLargestAssembly, NetLaunch, NetPart, RequestLaunch};
use bad_spaceship_shared::part::{Gimbal, Holdable, RocketEngine, SuppressLocalParts};
use bad_spaceship_shared::Character;
use bevy::prelude::*;
use bevy_egui::{
    egui::{self, Align2, Color32, Frame},
    EguiContexts,
};
use lightyear::prelude::{Connected, MessageSender, Predicted};
use std::collections::{HashMap, HashSet};

use crate::render_secondary_pass::main_assembly;
use crate::ui::EguiDrawSystems;

pub struct LaunchPlugin;

impl Plugin for LaunchPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LaunchLocal>()
            .add_systems(Update, tick_launch)
            .add_systems(
                bevy_egui::EguiPrimaryContextPass,
                show_launch_ui.in_set(EguiDrawSystems),
            )
            // Thrust is a continuous force → apply once per physics tick (an Update-rate
            // force would make the lift frame-rate-dependent). One path per mode: the
            // single-player sim vs. the predicted multiplayer parts.
            .add_systems(
                FixedUpdate,
                (
                    apply_sp_thrust.run_if(not(resource_exists::<SuppressLocalParts>)),
                    apply_mp_thrust.run_if(resource_exists::<SuppressLocalParts>),
                ),
            );
    }
}

/// Seconds the "Blastoff!" banner lingers after the count reaches zero.
const BLASTOFF_BANNER_SECS: f32 = 1.5;

/// Single-player countdown/launch phase. In multiplayer the server owns this (replicated
/// via [`NetLaunch`]), so `sp` stays `Idle` there and is unused.
#[derive(Default, PartialEq, Clone, Copy)]
enum SpPhase {
    #[default]
    Idle,
    Countdown {
        remaining: f32,
    },
    Launched,
}

#[derive(Resource, Default)]
struct LaunchLocal {
    /// Slide-to-launch progress `[0, 1]` while arming; springs back to 0 if released
    /// before the end (so launching takes one deliberate full swipe).
    slider: f32,
    /// "Blastoff!" banner timer (both modes).
    banner: f32,
    /// Single-player countdown/launch phase.
    sp: SpPhase,
    /// Multiplayer: whether we've already fired the banner for the current launch (so the
    /// replicated `launched` edge triggers the banner exactly once).
    mp_banner_fired: bool,
}

/// Advance the single-player countdown + the blastoff banner, and detect the multiplayer
/// blastoff edge. In single-player, the tick that crosses zero transitions to `Launched`
/// and cuts the local assembly's ground joints.
fn tick_launch(
    time: Res<Time>,
    mut local: ResMut<LaunchLocal>,
    mut commands: Commands,
    multiplayer: Option<Res<SuppressLocalParts>>,
    // Single-player ground-joint cut at blastoff.
    joints: Query<(Entity, &SphericalJoint)>,
    holdables: Query<Entity, With<Holdable>>,
    // Multiplayer launch state (replicated on the room's orb).
    orb: Query<&NetLaunch>,
) {
    let dt = time.delta_secs();
    if local.banner > 0.0 {
        local.banner = (local.banner - dt).max(0.0);
    }

    if multiplayer.is_some() {
        // Fire the banner once on the replicated `launched` rising edge.
        let launched = orb.iter().next().is_some_and(|l| l.launched);
        if launched && !local.mp_banner_fired {
            local.banner = BLASTOFF_BANNER_SECS;
            local.mp_banner_fired = true;
        } else if !launched {
            local.mp_banner_fired = false;
        }
        return;
    }

    if let SpPhase::Countdown { remaining } = local.sp {
        let remaining = remaining - dt;
        if remaining <= 0.0 {
            local.sp = SpPhase::Launched;
            local.banner = BLASTOFF_BANNER_SECS;
            cut_ground_joints(&mut commands, &joints, &holdables);
        } else {
            local.sp = SpPhase::Countdown { remaining };
        }
    }
}

/// Despawn every joint with an endpoint that isn't a `Holdable` part — a joint pinning a
/// part to the ground (the only other jointable body is the static ground, which isn't
/// `Holdable`). Part-to-part joints stay intact so the assembly holds together as it lifts.
fn cut_ground_joints(
    commands: &mut Commands,
    joints: &Query<(Entity, &SphericalJoint)>,
    holdables: &Query<Entity, With<Holdable>>,
) {
    let parts: HashSet<Entity> = holdables.iter().collect();
    for (entity, joint) in joints.iter() {
        if !parts.contains(&joint.body1) || !parts.contains(&joint.body2) {
            commands.entity(entity).despawn();
        }
    }
}

/// Apply balanced thrust to the single-player main assembly's rockets each physics tick.
fn apply_sp_thrust(
    time: Res<Time>,
    local: Res<LaunchLocal>,
    parts: Query<(Entity, &GlobalTransform, &ComputedMass), With<Holdable>>,
    joints: Query<&SphericalJoint>,
    // `Forces` takes `AngularVelocity` mutably inside (and writes each rocket's
    // `Gimbal` the geometry pass reads), so the spin/geometry reads and the force
    // write cannot coexist as sibling queries (B0001) — sequence them.
    mut set: ParamSet<(
        Query<&AngularVelocity>,
        Query<(Entity, &GlobalTransform, &Gimbal), With<RocketEngine>>,
        Query<(Entity, Forces, &mut Gimbal), With<RocketEngine>>,
    )>,
    gravity: Res<Gravity>,
) {
    if local.sp != SpPhase::Launched {
        return;
    }
    let Some((members, _)) = main_assembly(&parts, &joints) else {
        return;
    };
    // The assembly's COM + rotational state, via the shared measurement (see
    // `measure_assembly_spin`) so the trim matches the server/MP paths exactly.
    let Some((com, spin)) = ({
        let velocities = set.p0();
        let samples = || {
            parts.iter().filter(|(entity, ..)| members.contains(entity)).map(
                |(entity, transform, part_mass)| {
                    let angular = velocities.get(entity).map(|w| w.0).unwrap_or_default();
                    (transform.translation(), angular, part_mass.value())
                },
            )
        };
        measure_assembly_spin(samples)
    }) else {
        return;
    };
    let geometry: Vec<(Entity, Vec3, Quat, bevy::math::Vec2)> = set
        .p1()
        .iter()
        .filter(|(entity, ..)| members.contains(entity))
        .map(|(entity, transform, gimbal)| {
            let (_, rotation, translation) = transform.to_scale_rotation_translation();
            (entity, translation, rotation, gimbal.0)
        })
        .collect();
    apply_thrust(com, gravity.0, &geometry, &spin, time.delta_secs(), &mut set.p2());
}

/// Apply balanced thrust to the multiplayer assembly's **predicted** rockets each physics
/// tick while the room is launched. Membership + pose come from the replicated
/// `InLargestAssembly` markers and the predicted Avian `Position`/`Rotation` (not
/// `GlobalTransform`, which `lightyear_avian` drives out of the fixed schedule).
fn apply_mp_thrust(
    time: Res<Time>,
    orb: Query<&NetLaunch>,
    // `Forces` takes `AngularVelocity` mutably inside, so the member read and the
    // force write cannot coexist as sibling queries (B0001) — sequence them.
    mut set: ParamSet<(
        Query<
            (Entity, &Position, &Rotation, &AngularVelocity, &ComputedMass, Option<&Gimbal>),
            (With<NetPart>, With<Predicted>, With<InLargestAssembly>),
        >,
        Query<(Entity, Forces, &mut Gimbal), (With<RocketEngine>, With<Predicted>)>,
    )>,
    gravity: Res<Gravity>,
) {
    if !orb.iter().next().is_some_and(|l| l.launched) {
        return;
    }
    // The assembly's COM + rotational state, via the shared measurement (see
    // `measure_assembly_spin`) so the trim matches the server exactly; collect
    // the member rockets' poses alongside (`Gimbal` marks the rockets — it rides
    // `insert_rocket_physics`).
    let (measured, geometry) = {
        let members = set.p0();
        let samples = || {
            members
                .iter()
                .map(|(_, position, _, angular, part_mass, _)| {
                    (position.0, angular.0, part_mass.value())
                })
        };
        let geometry: Vec<(Entity, Vec3, Quat, bevy::math::Vec2)> = members
            .iter()
            .filter_map(|(entity, position, rotation, _, _, gimbal)| {
                gimbal.map(|g| (entity, position.0, rotation.0, g.0))
            })
            .collect();
        (measure_assembly_spin(samples), geometry)
    };
    let Some((com, spin)) = measured else {
        return;
    };
    apply_thrust(com, gravity.0, &geometry, &spin, time.delta_secs(), &mut set.p1());
}

/// Compute the balanced per-rocket commands for an assembly, slew each rocket's gimbal
/// toward its target, and apply the deflected force at its flare base through the
/// rockets' `Forces`. Shared by the single-player and multiplayer thrust systems (which
/// differ only in how they gather membership + pose).
fn apply_thrust(
    com: Vec3,
    gravity: Vec3,
    geometry: &[(Entity, Vec3, Quat, bevy::math::Vec2)],
    spin: &AssemblySpin,
    dt: f32,
    rocket_forces: &mut Query<(Entity, Forces, &mut Gimbal), impl bevy::ecs::query::QueryFilter>,
) {
    if geometry.is_empty() {
        return;
    }
    let to_apply: HashMap<Entity, (f32, bevy::math::Vec2)> =
        balanced_assembly_thrust(com, gravity, geometry, spin)
            .into_iter()
            .map(|thrust| (thrust.entity, (thrust.throttle, thrust.desired_gimbal)))
            .collect();
    let poses: HashMap<Entity, (Vec3, Quat)> = geometry
        .iter()
        .map(|&(entity, translation, rotation, _)| (entity, (translation, rotation)))
        .collect();
    let full = full_rocket_thrust(gravity);
    for (entity, mut forces, mut gimbal) in rocket_forces.iter_mut() {
        let Some(&(throttle, desired)) = to_apply.get(&entity) else {
            continue;
        };
        let (translation, rotation) = poses[&entity];
        gimbal.0 = gimbal_step(gimbal.0, desired, dt);
        let (point, force) = gimbaled_rocket_thrust(translation, rotation, full, throttle, gimbal.0);
        forces.apply_force_at_point(force, point);
    }
}

/// Draw the slide-to-launch control and the countdown / blastoff banner (top-centre), and
/// — on a completed swipe — start the launch (single-player: locally; multiplayer: send a
/// `RequestLaunch`).
fn show_launch_ui(
    mut contexts: EguiContexts,
    mut local: ResMut<LaunchLocal>,
    multiplayer: Option<Res<SuppressLocalParts>>,
    // Membership sources: single-player assembly, or the replicated multiplayer markers.
    sp_parts: Query<(Entity, &GlobalTransform, &ComputedMass), With<Holdable>>,
    sp_joints: Query<&SphericalJoint>,
    mp_members: Query<Entity, (With<InLargestAssembly>, With<Predicted>)>,
    orb: Query<&NetLaunch>,
    character: Query<Entity, With<Character>>,
    collisions: avian3d::prelude::Collisions,
    mut launch_sender: Query<&mut MessageSender<RequestLaunch>, With<Connected>>,
) -> Result {
    // Current countdown/launched state and whether we can still start a launch.
    let (counting, launched) = if multiplayer.is_some() {
        match orb.iter().next() {
            Some(l) => (l.remaining, l.launched),
            None => (0.0, false),
        }
    } else {
        match local.sp {
            SpPhase::Countdown { remaining } => (remaining, false),
            SpPhase::Launched => (0.0, true),
            SpPhase::Idle => (0.0, false),
        }
    };
    let idle = counting <= 0.0 && !launched;

    let ctx = contexts.ctx_mut()?;

    // Big centred countdown word, or the lingering "Blastoff!" banner.
    let banner = if local.banner > 0.0 {
        Some("Blastoff!".to_owned())
    } else if counting > 0.0 {
        Some(countdown_word(counting))
    } else {
        None
    };
    if let Some(text) = banner {
        egui::Area::new(egui::Id::new("bs_launch_banner"))
            .anchor(Align2::CENTER_TOP, egui::vec2(0.0, 84.0))
            .show(ctx, |ui| {
                // Let the big word size to its natural width instead of wrapping "Blastoff!"
                // onto several lines inside the anchored (width-less) area.
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.label(
                    egui::RichText::new(text)
                        .size(64.0)
                        .strong()
                        .color(Color32::from_rgb(255, 220, 80)),
                );
            });
    }

    // The slider only shows while idle and the character is touching the assembly.
    let available = idle && character_touches_assembly(
        &character,
        &collisions,
        &assembly_members(multiplayer.is_some(), &sp_parts, &sp_joints, &mp_members),
    );
    if !available {
        // Keep the slider reset whenever it isn't shown.
        local.slider = 0.0;
        return Ok(());
    }

    let mut progress = local.slider;
    let mut arm = false;
    egui::Area::new(egui::Id::new("bs_launch_slider"))
        .anchor(Align2::CENTER_TOP, egui::vec2(0.0, 24.0))
        .show(ctx, |ui| {
            Frame::default()
                .fill(Color32::from_black_alpha(160))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.colored_label(Color32::from_rgb(255, 220, 80), "Slide to launch →");
                        let response =
                            ui.add(egui::Slider::new(&mut progress, 0.0..=1.0).show_value(false));
                        if progress >= 0.999 {
                            arm = true;
                        } else if !response.dragged() {
                            // Released before the end — spring back so arming needs one
                            // uninterrupted swipe.
                            progress = 0.0;
                        }
                    });
                });
        });

    if arm {
        local.slider = 0.0;
        if multiplayer.is_some() {
            if let Ok(mut sender) = launch_sender.single_mut() {
                sender.send::<ControlChannel>(RequestLaunch);
            }
        } else {
            local.sp = SpPhase::Countdown { remaining: LAUNCH_COUNTDOWN_SECS };
        }
    } else {
        local.slider = progress;
    }
    Ok(())
}

/// The main-assembly member entities, from the mode's authoritative source: single-player
/// [`main_assembly`], or the replicated multiplayer `InLargestAssembly` markers.
fn assembly_members(
    multiplayer: bool,
    sp_parts: &Query<(Entity, &GlobalTransform, &ComputedMass), With<Holdable>>,
    sp_joints: &Query<&SphericalJoint>,
    mp_members: &Query<Entity, (With<InLargestAssembly>, With<Predicted>)>,
) -> HashSet<Entity> {
    if multiplayer {
        mp_members.iter().collect()
    } else {
        main_assembly(sp_parts, sp_joints).map(|(members, _)| members).unwrap_or_default()
    }
}

/// Whether the character's body is in contact with any part of the assembly.
fn character_touches_assembly(
    character: &Query<Entity, With<Character>>,
    collisions: &avian3d::prelude::Collisions,
    members: &HashSet<Entity>,
) -> bool {
    let Ok(character) = character.single() else {
        return false;
    };
    if members.is_empty() {
        return false;
    }
    collisions
        .collisions_with(character)
        .filter(|pair| pair.is_touching())
        .any(|pair| {
            let other = if pair.collider1 == character {
                pair.collider2
            } else {
                pair.collider1
            };
            members.contains(&other)
        })
}

/// The countdown word for a given remaining time: `"3"` while `2 < t ≤ 3`, `"2"` while
/// `1 < t ≤ 2`, `"1"` while `0 < t ≤ 1`.
fn countdown_word(remaining: f32) -> String {
    (remaining.ceil().max(1.0) as i32).to_string()
}
