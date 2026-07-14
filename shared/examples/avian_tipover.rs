// Faithful avian reproduction of the tip-over: the recorded 4-part pod spawned as
// REAL dynamic bodies + spherical-joint welds in avian's actual solver (matching the
// server: PhysicsPlugins::default(), SubstepCount(6), 1/60 fixed step), driven by the
// REAL `assembly_burn` control law each FixedUpdate. Command = hold straight up.
//
// This bridges the gap the rigid-body sim couldn't: here the assembly is 4 separate
// bodies held by compliant welds, exactly like the game. BS_RIDER=1 adds a
// rotation-locked rider welded to the deck.
use avian3d::prelude::*;
use avian3d::PhysicsPlugins;
use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;
use core::time::Duration;

use bad_spaceship_shared::guidance::Guidance;
use bad_spaceship_shared::launch::{assembly_burn, measure_assembly_spin};
use bad_spaceship_shared::part::{
    insert_part_physics, insert_rocket_physics, Gimbal, RocketEngine, ROCKET_THRUST_DIR_LOCAL,
};

const DT: f32 = 1.0 / 60.0;

const POSES: [(Vec3, Quat); 4] = [
    (Vec3::new(0.7222, 0.1565, 0.0196), Quat::from_xyzw(0.020174, -0.929430, 0.008549, 0.368346)),
    (Vec3::new(-0.2419, 0.1744, 1.2214), Quat::from_xyzw(-0.015187, 0.159457, -0.008092, 0.987055)),
    (Vec3::new(1.4470, 0.2453, 1.8026), Quat::from_xyzw(0.004966, -0.993299, 0.022028, -0.113344)),
    (Vec3::new(0.7914, 1.2220, 0.9222), Quat::from_xyzw(0.099959, 0.127962, 0.704025, 0.691363)),
];

#[derive(Resource, Default)]
struct Ctl {
    integral: Vec3,
    engines: Vec<Entity>,
    burns: Vec<(Entity, Vec2, Vec3, Vec3)>,
    tick: u64,
}

fn setup(mut commands: Commands, mut ctl: ResMut<Ctl>) {
    let slab_half = Vec3::new(0.12142, 1.45986, 1.26851);
    let welds: [((usize, usize), Vec3, Vec3); 12] = [
        ((1, 0), Vec3::new(0.6972, -1.6061, -0.4285), Vec3::new(0.8123, -1.5928, -0.0965)),
        ((3, 0), Vec3::new(-0.1181, -0.4799, -1.1648), Vec3::new(-0.3817, 0.8967, 0.1193)),
        ((3, 0), Vec3::new(-0.1342, 0.1141, -0.6299), Vec3::new(0.3813, 0.9128, -0.1193)),
        ((3, 0), Vec3::new(-0.1262, 0.0849, -1.1946), Vec3::new(0.1192, 0.9047, 0.3818)),
        ((3, 0), Vec3::new(-0.1262, -0.4506, -0.6002), Vec3::new(-0.1196, 0.9047, -0.3817)),
        ((3, 2), Vec3::new(-0.1183, 0.0488, 1.0407), Vec3::new(0.3173, 0.8969, -0.2434)),
        ((3, 2), Vec3::new(-0.1267, -0.7467, 0.9579), Vec3::new(-0.3173, 0.9053, 0.2434)),
        ((3, 2), Vec3::new(-0.1225, -0.3075, 0.6015), Vec3::new(0.2435, 0.9011, 0.3174)),
        ((3, 1), Vec3::new(-0.1203, 1.4216, -0.241), Vec3::new(-0.3646, 0.8988, -0.1643)),
        ((3, 1), Vec3::new(-0.1344, 0.6936, 0.0896), Vec3::new(0.3643, 0.913, 0.1642)),
        ((3, 1), Vec3::new(-0.1273, 0.8922, -0.4399), Vec3::new(0.1642, 0.9059, -0.3648)),
        ((3, 1), Vec3::new(-0.1273, 1.223, 0.2885), Vec3::new(-0.1645, 0.9059, 0.3646)),
    ];

    let mut ent = Vec::new();
    for (i, (pos, rot)) in POSES.iter().enumerate() {
        let mut e = commands.spawn((Position(*pos), Rotation(*rot)));
        if i < 3 {
            insert_rocket_physics(&mut e);
        } else {
            insert_part_physics(&mut e, slab_half);
        }
        ent.push(e.id());
    }
    for ((a, b), aa, bb) in welds {
        commands.spawn(SphericalJoint::new(ent[a], ent[b]).with_local_anchor1(aa).with_local_anchor2(bb));
    }
    if std::env::var("BS_RIDER").ok().as_deref() == Some("1") {
        let parts_com = POSES.iter().map(|(p, _)| *p).sum::<Vec3>() / 4.0;
        let rider_pos = parts_com + Vec3::new(0.49, 1.61, 0.15);
        let rider = commands
            .spawn((
                RigidBody::Dynamic,
                Collider::capsule(0.33, 0.33),
                Mass(1.0),
                LockedAxes::ROTATION_LOCKED,
                Position(rider_pos),
                Rotation::default(),
            ))
            .id();
        // The game's real lock weld: CENTER-anchored on the avatar (Vec3::ZERO) to the
        // touched part (see `avatar_lock_contacts`). One weld per touched part.
        let slab_inv = POSES[3].1.inverse();
        commands.spawn(
            SphericalJoint::new(rider, ent[3])
                .with_local_anchor1(Vec3::ZERO)
                .with_local_anchor2(slab_inv * (rider_pos - POSES[3].0)),
        );
        println!("rider=true");
    } else {
        println!("rider=false");
    }
    ctl.engines = ent[..3].to_vec();
    println!("{:>6} {:>7} {:>8} {:>8}", "t", "tilt", "omega", "comY");
}

fn control_compute(
    mut ctl: ResMut<Ctl>,
    engines: Query<(Entity, &Position, &Rotation, &Gimbal), With<RocketEngine>>,
    parts: Query<(&Position, &LinearVelocity, &AngularVelocity, &ComputedMass)>,
) {
    let g = Vec3::NEG_Y * 9.81;
    let geometry: Vec<(Entity, Vec3, Quat, Vec2)> = ctl
        .engines
        .iter()
        .filter_map(|e| engines.get(*e).ok().map(|(en, p, r, gi)| (en, p.0, r.0, gi.0)))
        .collect();
    if geometry.len() < 3 {
        return;
    }
    let samples = || parts.iter().map(|(p, l, a, m)| (p.0, l.0, a.0, m.value()));
    let Some((com, spin)) = measure_assembly_spin(samples) else { return };
    let mut integral = ctl.integral;
    let burns = assembly_burn(com, g, DT, &geometry, &spin, &mut integral, Guidance { thrust_dir: Vec3::Y, throttle: 1.0 });
    ctl.integral = integral;
    ctl.burns = burns.into_iter().map(|b| (b.entity, b.gimbal, b.force, b.point)).collect();
}

fn control_apply(ctl: Res<Ctl>, mut engines: Query<(Forces, &mut Gimbal), With<RocketEngine>>) {
    for (entity, gimbal, force, point) in &ctl.burns {
        if let Ok((mut forces, mut gim)) = engines.get_mut(*entity) {
            gim.0 = *gimbal;
            forces.apply_force_at_point(*force, *point);
        }
    }
}

fn log_tilt(
    mut ctl: ResMut<Ctl>,
    engines: Query<(&Rotation, &AngularVelocity), With<RocketEngine>>,
    parts: Query<&Position>,
) {
    ctl.tick += 1;
    if ctl.tick % 30 != 0 {
        return;
    }
    let (mut up, mut omega, mut n) = (Vec3::ZERO, Vec3::ZERO, 0.0);
    for (r, a) in &engines {
        up += r.0 * ROCKET_THRUST_DIR_LOCAL;
        omega += a.0;
        n += 1.0;
    }
    up = (up / n).normalize_or(Vec3::Y);
    let tilt = up.y.clamp(-1.0, 1.0).acos().to_degrees();
    let count = parts.iter().count().max(1) as f32;
    let com_y = parts.iter().map(|p| p.0.y).sum::<f32>() / count;
    println!("{:>6.1} {:>7.1} {:>8.3} {:>8.1}", ctl.tick as f32 * DT, tilt, (omega / n).length(), com_y);
}

fn main() {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, TransformPlugin, PhysicsPlugins::default()));
    app.insert_resource(Gravity(Vec3::NEG_Y * 9.81));
    app.insert_resource(SubstepCount(6));
    app.insert_resource(Time::<Fixed>::from_duration(Duration::from_secs_f32(DT)));
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f32(DT)));
    app.init_resource::<Ctl>();
    app.add_systems(Startup, setup);
    app.add_systems(FixedUpdate, (control_compute, control_apply, log_tilt).chain());
    app.finish();
    for _ in 0..(35 * 60) {
        app.update();
    }
}
