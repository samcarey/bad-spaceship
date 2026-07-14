// Isolated reproduction of the small-stack tip-over. Drive the REAL `assembly_burn`
// control law with the recorded 4-part pod (3 engines + 1 slab) in a rigid-body sim.
//
// Key fidelity points that separate the PLANT (true physics) from the CONTROLLER's
// model, mirroring the server:
//   * PLANT inertia = point masses + the slab's own box inertia + (optionally) a rider
//     mass locked on top. This is what avian actually integrates.
//   * CONTROLLER (`measure_assembly_spin`) sees only the 4 PARTS as POINT masses — no
//     part self-inertia, no rider. So it underestimates inertia and misplaces the COM.
//
// Env knobs: BS_RIDER=1 adds the recorded locked rider; BS_SLAB_INERTIA=0 drops the
// slab self-inertia from the plant (to isolate each effect).
use bevy::math::{Mat3, Quat, Vec2, Vec3};
use bad_spaceship_shared::guidance::Guidance;
use bad_spaceship_shared::launch::{
    assembly_burn, full_rocket_thrust, measure_assembly_spin, AssemblySpin,
};
use bad_spaceship_shared::part::{PART_DENSITY, ROCKET_THRUST_DIR_LOCAL, ROCKET_VOLUME};

fn box_inertia(m: f32, full: Vec3, rot: Quat) -> Mat3 {
    // Solid box inertia about its own center, then rotated into the body frame.
    let (a, b, c) = (full.x, full.y, full.z);
    let d = Mat3::from_diagonal(Vec3::new(
        m / 12.0 * (b * b + c * c),
        m / 12.0 * (a * a + c * c),
        m / 12.0 * (a * a + b * b),
    ));
    let r = Mat3::from_quat(rot);
    r * d * r.transpose()
}

fn main() {
    let g = Vec3::new(0.0, -9.81, 0.0);
    let rider_on = std::env::var("BS_RIDER").ok().as_deref() == Some("1");
    let slab_inertia_on = std::env::var("BS_SLAB_INERTIA").ok().as_deref() != Some("0");

    let engines = [
        (Vec3::new(0.7222, 0.1565, 0.0196), Quat::from_xyzw(0.020174, -0.929430, 0.008549, 0.368346)),
        (Vec3::new(-0.2419, 0.1744, 1.2214), Quat::from_xyzw(-0.015187, 0.159457, -0.008092, 0.987055)),
        (Vec3::new(1.4470, 0.2453, 1.8026), Quat::from_xyzw(0.004966, -0.993299, 0.022028, -0.113344)),
    ];
    let slab_pos = Vec3::new(0.7914, 1.2220, 0.9222);
    let slab_rot = Quat::from_xyzw(0.099959, 0.127962, 0.704025, 0.691363);
    let slab_full = Vec3::new(0.12142 * 2.0, 1.45986 * 2.0, 1.26851 * 2.0);
    let m_eng = ROCKET_VOLUME * PART_DENSITY;
    let m_slab = slab_full.x * slab_full.y * slab_full.z * PART_DENSITY;

    // ---- CONTROLLER's model: 4 parts as point masses (matches measure_assembly_spin). ----
    let ctrl_pos: Vec<Vec3> = engines.iter().map(|(p, _)| *p).chain(std::iter::once(slab_pos)).collect();
    let ctrl_mass = [m_eng, m_eng, m_eng, m_slab];
    let eng_rot: Vec<Quat> = engines.iter().map(|(_, q)| *q).collect();

    // ---- PLANT: same parts + slab self-inertia + optional rider on top. ----
    let mut plant_pos = ctrl_pos.clone();
    let mut plant_mass = ctrl_mass.to_vec();
    if rider_on {
        // Recorded rider offset from the parts-COM, mass 1.0 (avatar Mass(1.0)).
        let parts_com: Vec3 =
            ctrl_pos.iter().zip(ctrl_mass).map(|(p, m)| *p * m).sum::<Vec3>() / ctrl_mass.iter().sum::<f32>();
        plant_pos.push(parts_com + Vec3::new(0.49, 1.61, 0.15));
        plant_mass.push(1.0);
    }
    let total_mass: f32 = plant_mass.iter().sum();
    let plant_com: Vec3 = plant_pos.iter().zip(&plant_mass).map(|(p, m)| *p * *m).sum::<Vec3>() / total_mass;
    let off: Vec<Vec3> = plant_pos.iter().map(|p| *p - plant_com).collect();
    let mut ibody = Mat3::ZERO;
    for (r, m) in off.iter().zip(&plant_mass) {
        ibody += Mat3::from_diagonal(Vec3::splat(*m * r.length_squared()))
            - Mat3::from_cols(*r * r.x, *r * r.y, *r * r.z) * *m;
    }
    if slab_inertia_on {
        ibody += box_inertia(m_slab, slab_full, slab_rot);
    }

    let mut q = Quat::IDENTITY;
    let mut com = Vec3::ZERO;
    let mut vel = Vec3::ZERO;
    let mut omega = std::env::var("BS_W0")
        .ok()
        .and_then(|s| {
            let v: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            (v.len() == 3).then(|| Vec3::new(v[0], v[1], v[2]))
        })
        .unwrap_or(Vec3::ZERO);
    let mut integral = Vec3::ZERO;
    let mut gimbals = [Vec2::ZERO; 3];
    let full = full_rocket_thrust(g);
    let dt = 1.0 / 60.0;
    // Emulate control authority lost through the compliant joint network (avian solves
    // the assembly as separate bodies; a rigid-body sim delivers 100%). BS_TORQUE_SCALE<1.
    let torque_scale: f32 = std::env::var("BS_TORQUE_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let up0 = eng_rot.iter().map(|r| *r * ROCKET_THRUST_DIR_LOCAL).sum::<Vec3>().normalize();

    println!(
        "rider={rider_on} slab_inertia={slab_inertia_on} TWR={:.2} plantI=({:.1},{:.1},{:.1})",
        3.0 * full / (total_mass * g.length()),
        ibody.x_axis.x, ibody.y_axis.y, ibody.z_axis.z
    );
    println!("{:>6} {:>7} {:>8} {:>8}", "t", "tilt", "omega", "comY");

    // Body-frame offsets of the CONTROLLER parts (relative to plant COM, since the rigid
    // body rotates about the plant COM).
    let ctrl_off: Vec<Vec3> = ctrl_pos.iter().map(|p| *p - plant_com).collect();

    for step in 0..(35 * 60) {
        let world_pos: Vec<Vec3> = ctrl_off.iter().map(|o| com + q * *o).collect();
        let world_rot: Vec<Quat> = eng_rot.iter().map(|r| q * *r).collect();
        let geometry: Vec<(bevy::ecs::entity::Entity, Vec3, Quat, Vec2)> = (0..3)
            .map(|i| (bevy::ecs::entity::Entity::PLACEHOLDER, world_pos[i], world_rot[i], gimbals[i]))
            .collect();
        // Controller measures spin over the 4 PARTS only (point masses).
        let samples = || {
            (0..4).map(|i| {
                let r = world_pos[i] - com;
                (world_pos[i], vel + omega.cross(r), omega, ctrl_mass[i])
            })
        };
        let (com_meas, spin) = measure_assembly_spin(samples).unwrap();
        let spin = AssemblySpin { angular_velocity: omega, ..spin };
        let guidance = Guidance { thrust_dir: Vec3::Y, throttle: 1.0 };
        let burns = assembly_burn(com_meas, g, dt, &geometry, &spin, &mut integral, guidance);

        // Plant: apply forces at engine points, torque about the TRUE plant COM.
        let mut force = Vec3::ZERO;
        let mut torque = Vec3::ZERO;
        for (i, b) in burns.iter().enumerate() {
            force += b.force;
            torque += (b.point - com).cross(b.force);
            gimbals[i] = b.gimbal;
        }
        vel += (force / total_mass + g) * dt;
        com += vel * dt;
        let rot = Mat3::from_quat(q);
        let iworld = rot * ibody * rot.transpose();
        let alpha = iworld.inverse() * (torque * torque_scale - omega.cross(iworld * omega));
        omega += alpha * dt;
        let wq = Quat::from_xyzw(omega.x, omega.y, omega.z, 0.0) * q;
        q = Quat::from_xyzw(
            q.x + 0.5 * wq.x * dt, q.y + 0.5 * wq.y * dt,
            q.z + 0.5 * wq.z * dt, q.w + 0.5 * wq.w * dt,
        ).normalize();

        if step % 30 == 0 {
            let up = (q * up0).normalize();
            let tilt = up.y.clamp(-1.0, 1.0).acos().to_degrees();
            println!("{:>6.1} {:>7.1} {:>8.3} {:>8.1}", step as f32 * dt, tilt, omega.length(), com.y);
        }
    }
}
