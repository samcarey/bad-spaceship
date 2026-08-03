//! Fuel-optimal launch guidance: which way the autopilot points the assembly's thrust,
//! and when to cut it, to leave the planet on the least fuel.
//!
//! **The physics.** Burning fuel does *not* reduce an assembly's mass in this sim (parts
//! carry a fixed density-mass), so there is no rocket-equation trade — the propellant cost
//! of a burn is just its **impulse**, `∫|thrust| dt`. What a burn *wastes* is **gravity
//! loss**: while thrust fights gravity head-on, `1/TWR` of the propellant does nothing but
//! hold the ship up. A **gravity turn** — a small pitchover kick at liftoff, then thrust
//! held prograde while gravity bends the arc over — banks horizontal speed that gravity
//! can't tax, so it beats a vertical climb by a margin that explodes as TWR → 1 (a
//! hovering-slow vertical ascent wastes nearly everything) and vanishes as TWR grows (a
//! strong stack is out of the well before gravity loss matters). With engines at real
//! first-stage strength (see `ROCKET_THRUST_PART_WEIGHTS`), a typical cargo-laden build
//! sits at TWR ~1.2–1.5 where the turn saves 15–35% — the turn is how you fly efficiently
//! — while an engine-dense, low-payload stack reaches TWR ≳2.5 where straight up is fine.
//!
//! **The law** (a pure function of the vehicle's live true state, so the live autopilot,
//! the trajectory preview, and the optimizer's forward sim can never disagree):
//!
//! 1. **Pitchover kick** below [`TURN_SPEED`]: thrust starts radial-out and tips toward
//!    [`DOWNRANGE_AZIMUTH`] by up to the policy angle as speed builds — the one thing a
//!    prograde law can't do on its own (straight up is an unstable equilibrium).
//! 2. **Gravity turn** above it: thrust prograde; the arc shape then adapts continuously
//!    to the vehicle's actual thrust, mass, and the weakening gravity field.
//! 3. **Escape cutoff**: throttle to zero the instant `E = ½v² − μ/r ≥ 0` — past that the
//!    ship coasts away on its own, so every further second of burn is wasted.
//!
//! **The one free parameter** — the pitchover angle — is chosen per assembly by
//! [`optimize_pitchover`]: a point-mass forward-sim sweep from the vehicle's launch state
//! (its real thrust-to-weight), constrained by a ground floor (a too-aggressive turn arcs
//! back into the terrain before escaping) and backed off from the crash boundary for
//! attitude-lag margin. Weak stacks get a gentle lean (~5–15°), strong stacks a hard one
//! (or fly nearly straight); the server replicates the whole planning seed
//! ([`LaunchSeed`]) so predicted clients rebuild the identical program — replicating just
//! the chosen angle left each peer sampling its table from its own state, which is not
//! the same program.

use crate::map::{gravity_at, GRAVITY_MU, GRAVITY_REF_RADIUS, PLANET_CENTER};
use bevy::math::Vec3;

/// Radius (m from centre) the forward sim treats as the ground: a trajectory that sinks
/// back below the launch pad has crashed, not escaped. Without this floor the optimizer
/// would pick a near-horizontal launch that builds orbital speed while gravity pulls it
/// *through* the planet — the floor is what forces a real gravity turn: climb enough to
/// keep clear, then lean.
pub const GROUND_RADIUS: f32 = GRAVITY_REF_RADIUS - 2.0;

/// Speed (m/s) below which the autopilot holds the vertical/pitchover attitude rather
/// than steering prograde: under this the velocity direction is too noisy to chase and
/// the vehicle is still clearing the pad.
pub const TURN_SPEED: f32 = 40.0;

/// The fixed horizontal direction the pitchover leans toward. The planet is spherically
/// symmetric so the choice is arbitrary; shared so the server and its predicted clients
/// turn the same way.
pub const DOWNRANGE_AZIMUTH: Vec3 = Vec3::X;

/// The optimizer's arc must hold this much altitude margin over the pad (latched — see
/// [`propagate`]): the real vehicle flies the plan imperfectly (attitude lag, rider trim),
/// and an ideal arc that grazes the terrain leaves no room for that. Module-scoped so the
/// tests that ask whether the optimizer's verdict is self-consistent grade against its own
/// ruler rather than a twin literal.
pub const OPTIMIZER_CLEARANCE_M: f32 = 400.0;

/// The autopilot's command for a tick: which way to point the net thrust (unit vector)
/// and how hard to burn (`throttle` ∈ {0, 1} — `0` once escape energy is reached).
#[derive(Clone, Copy, Debug)]
pub struct Guidance {
    pub thrust_dir: Vec3,
    pub throttle: f32,
}

/// The point-mass vehicle the planning sims fly: full-throttle thrust acceleration
/// (m/s²) and total mass (kg — converts the shared [`crate::map::drag_force`] into a
/// deceleration). Always derived together from the same assembly; bundled because as
/// adjacent bare `f32`s a swapped call site compiles clean into a plausibly-wrong plan.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Vehicle {
    pub thrust_accel: f32,
    pub mass: f32,
}

impl Vehicle {
    /// The point-mass vehicle for a stack of `engines` rockets at `total_mass`, at the
    /// **derated** thrust every planning sim flies (the allocator's `LIFT_FLOOR` — see
    /// the rationale in [`PitchProgram::plan`]). The one constructor shared by the
    /// planner and the live trajectory preview, so the previewed arc is sampled from
    /// exactly the vehicle the plan was optimized for. `total_mass` must be positive.
    pub fn derated(engines: usize, gravity: Vec3, total_mass: f32) -> Self {
        Self {
            thrust_accel: crate::launch::LIFT_FLOOR
                * engines as f32
                * crate::launch::full_rocket_thrust(gravity)
                / total_mass,
            mass: total_mass,
        }
    }
}

/// Specific orbital energy `½v² − μ/r` (J/kg) at a true world position + velocity.
/// `≥ 0` means the assembly is on an escape trajectory — it will leave the planet even
/// with the engines off.
pub fn specific_energy(true_pos: Vec3, true_vel: Vec3) -> f32 {
    let r = (true_pos - PLANET_CENTER).length().max(1.0);
    0.5 * true_vel.length_squared() - GRAVITY_MU / r
}

/// Whether the assembly has reached escape energy (`E ≥ 0`).
pub fn escaped(true_pos: Vec3, true_vel: Vec3) -> bool {
    specific_energy(true_pos, true_vel) >= 0.0
}

/// Whether escape is **secured**: energy ≥ 0 AND on the outbound leg (radial velocity
/// ≥ 0). Scalar escape energy alone is NOT a safe cutoff: a near-horizontal burn can
/// reach `E ≥ 0` while the path still points slightly downward — energy is conserved
/// through the coast, but the hyperbola's periapsis can lie below the surface, and the
/// "escaped" ship dives back to the ground (observed live: tilt to horizontal, then the
/// altimeter unwinding to zero). Burning through the dip until the path turns outbound
/// costs a little extra fuel and guarantees the coast never descends again: with
/// `E ≥ 0` and `v·r̂ ≥ 0`, r grows monotonically forever.
pub fn escape_secured(true_pos: Vec3, true_vel: Vec3) -> bool {
    escaped(true_pos, true_vel) && (true_pos - PLANET_CENTER).dot(true_vel) >= 0.0
}

/// The commanded thrust direction (unit) for the ascent law at a true position +
/// velocity: radial-out tipped by the blended `pitchover` below [`TURN_SPEED`], prograde
/// (a gravity turn) above it. `pitchover` of 0 is a pure vertical ascent.
pub fn ascent_thrust_dir(true_pos: Vec3, true_vel: Vec3, pitchover: f32) -> Vec3 {
    let up = (true_pos - PLANET_CENTER).normalize_or(Vec3::Y);
    let speed = true_vel.length();
    if speed >= TURN_SPEED && pitchover.abs() >= 1e-4 {
        // Gravity turn: follow velocity. `normalize_or(up)` guards the (unreachable at
        // this speed) zero-velocity case.
        return true_vel.normalize_or(up);
    }
    // Pitchover kick: tip from radial toward the downrange azimuth, the tilt growing
    // linearly to `pitchover` as speed approaches TURN_SPEED, so the kick is a ramp the
    // attitude loop can track rather than a step.
    tilt_downrange(up, pitchover * (speed / TURN_SPEED))
}

/// Radial-up tipped by `angle` toward [`DOWNRANGE_AZIMUTH`] — the single source of the
/// "lean the command downrange" construction, shared by the planning law and the flown
/// program so the two can never disagree about what an angle means.
fn tilt_downrange(up: Vec3, angle: f32) -> Vec3 {
    if angle.abs() < 1e-4 {
        return up; // vertical: never chases a sideways disturbance
    }
    let horiz = (DOWNRANGE_AZIMUTH - up * up.dot(DOWNRANGE_AZIMUTH)).normalize_or_zero();
    (up * angle.cos() + horiz * angle.sin()).normalize_or(up)
}

/// The complete input set a [`PitchProgram`] is sampled from — every value
/// [`PitchProgram::build`] reads, and nothing else.
///
/// It exists as one replicated struct because the program is **not** reproducible from
/// "the same rules applied to my own state": [`PitchProgram::build`] forward-simulates
/// the ideal ascent and records a table at ~1 m/s knots, so two peers seeding it from
/// their own live state a few ticks apart get *different tables*, and the interpolated
/// command then stands apart for the entire burn. Measured on a ridden 4-rocket launch:
/// the server planned at its blastoff tick and the client 7 ticks later (once the
/// replicated angle arrived), and their programs disagreed by **1.2 mrad at the same
/// speed** — a standing ~0.07 N lateral thrust error that drifted the two simulations
/// apart quadratically until the position tolerance corrected them, over and over.
/// Replicating the angle alone was not enough; the *seed* is the state.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LaunchSeed {
    /// The pitchover angle (rad) the optimizer chose for this launch.
    pub pitchover: f32,
    /// The assembly's TRUE (frame-folded) position at the planning tick.
    pub position: [f32; 3],
    /// The assembly's TRUE (frame-folded) velocity at the planning tick.
    pub velocity: [f32; 3],
    /// The derated point-mass vehicle the plan was optimized for.
    pub vehicle: Vehicle,
}

/// A **pitch program**: the ascent command precomputed as "flight-path angle vs speed",
/// sampled off the ideal point-mass trajectory at launch. The live autopilot flies *this*
/// instead of chasing the raw prograde direction.
///
/// Why: closed-loop prograde steering tracks a target that moves every tick (the live
/// velocity vector, noise included), so the throttle/gimbal allocator trims continuously
/// for the whole climb — recorded A/B flights measured ~5–7% of the burn lost to that
/// chatter, *more* than a gentle turn saves, regardless of how small the kick angle was.
/// Real launch vehicles solve this exactly this way: guidance is an open-loop pitch
/// program, and only *attitude* is closed-loop. The program's command varies smoothly and
/// slowly (it's indexed by the vehicle's own speed, monotonic during a burn), so holding
/// it costs the attitude loop no more than holding straight-up — while the trajectory
/// still bends into the fuel-saving arc.
#[derive(Debug, Default)]
pub struct PitchProgram {
    /// Everything this program was sampled from — replicated whole so a predicted
    /// client rebuilds the identical table (see [`LaunchSeed`]).
    pub seed: LaunchSeed,
    /// `(speed m/s, command angle from radial-up rad)`, speeds strictly ascending —
    /// the ideal law's own command schedule along its trajectory.
    samples: Vec<(f32, f32)>,
}

impl PitchProgram {
    /// The one shared constructor every thrust site plans through (the
    /// `measure_assembly_spin` discipline): from the assembly's true launch state, its
    /// engine count and **total mass — riders included** (the launch gate guarantees they
    /// fly with the stack; a parts-only plan diverges visibly from the real arc), derive
    /// thrust acceleration, pick the pitchover (`forced` overrides the optimizer — the
    /// server's `BS_FORCE_PITCHOVER_DEG` hook and the MP client rebuilding from the
    /// replicated angle both land here), and sample the program. A non-positive mass
    /// yields the straight-up default rather than a garbage gravity-only plan.
    pub fn plan(
        true_pos: Vec3,
        true_vel: Vec3,
        engines: usize,
        gravity: Vec3,
        total_mass: f32,
        forced: Option<f32>,
    ) -> Self {
        if total_mass <= 0.0 {
            return Self::default();
        }
        // Plan against DERATED thrust: the throttle allocator trims engines for attitude
        // (lift floor 0.85, deeper transients with a rider's standing torque), so the
        // realized mean thrust is below nominal. Planning at the floor puts reality on
        // the SAFE side of the plan — a better-than-planned burn climbs above the
        // planned arc, never below it (a side-hung rider once realized ~20% less thrust
        // than a nominal plan assumed, and the 23° arc sagged kilometres under the
        // surface line).
        // == the allocator's `LIFT_FLOOR` on purpose (planning at the realized-thrust
        // floor keeps reality on the safe side of the plan); one source, not a twin literal.
        // The planning model is drag-aware: the optimizer and the sampled program both
        // fly against the same [`crate::map::drag_force`] the physics applies (divided by
        // the assembly mass), so the flight plan already leans/burns to compensate for
        // the air rather than discovering it in flight.
        let vehicle = Vehicle::derated(engines, gravity, total_mass);
        let pitchover =
            forced.unwrap_or_else(|| optimize_pitchover(true_pos, true_vel, vehicle));
        Self::build(LaunchSeed {
            pitchover,
            position: true_pos.to_array(),
            velocity: true_vel.to_array(),
            vehicle,
        })
    }

    /// Build the program by flying the ideal ascent law ([`ascent_thrust_dir`]) as a
    /// point mass from the launch state and recording its command angle at each speed.
    /// A zero `pitchover` yields an all-zero program (straight up), so high-TWR stacks
    /// are byte-identical to the fixed vertical command. The [`Vehicle`]'s mass converts
    /// the shared [`crate::map::drag_force`] into a deceleration so the sampled arc
    /// matches the real drag-braked climb.
    ///
    /// Takes the whole [`LaunchSeed`] rather than loose arguments precisely so a
    /// multiplayer client can rebuild a launch's program from the replicated seed with
    /// **no** input of its own — see that type for what re-planning locally cost.
    pub fn build(seed: LaunchSeed) -> Self {
        let (vehicle, pitchover) = (seed.vehicle, seed.pitchover);
        let mut samples = Vec::new();
        let mut pos = Vec3::from_array(seed.position);
        let mut vel = Vec3::from_array(seed.velocity);
        for _ in 0..OPTIMIZER_STEPS {
            let dir = ascent_thrust_dir(pos, vel, pitchover);
            let up = (pos - PLANET_CENTER).normalize_or(Vec3::Y);
            let speed = vel.length();
            let angle = dir.dot(up).clamp(-1.0, 1.0).acos();
            if samples.last().is_none_or(|&(s, _)| speed > s + 1.0) {
                samples.push((speed, angle));
            }
            if escape_secured(pos, vel) {
                break;
            }
            let accel = dir * vehicle.thrust_accel
                + gravity_at(pos)
                + crate::map::drag_force(pos, vel) / vehicle.mass;
            vel += accel * OPTIMIZER_DT;
            pos += vel * OPTIMIZER_DT;
            // Ideal trajectory dove below the pad (an over-aggressive angle the optimizer
            // shouldn't have picked): stop sampling rather than record a descent.
            if (pos - PLANET_CENTER).length() < GROUND_RADIUS {
                break;
            }
        }
        Self { seed, samples }
    }

    /// The command angle (rad from radial-up) at a given speed — linear interpolation,
    /// clamped to the table ends (binary search; the speeds are strictly ascending).
    /// Empty table = straight up.
    pub fn angle_at(&self, speed: f32) -> f32 {
        let i = self.samples.partition_point(|&(s, _)| s < speed);
        match (i.checked_sub(1).and_then(|j| self.samples.get(j)), self.samples.get(i)) {
            (Some(&(s0, a0)), Some(&(s1, a1))) => {
                a0 + (a1 - a0) * ((speed - s0) / (s1 - s0)).clamp(0.0, 1.0)
            }
            (Some(&(_, a)), None) | (None, Some(&(_, a))) => a, // past either table end
            (None, None) => 0.0,
        }
    }

    /// Diagnostic fingerprint of the sampled table: `(sample count, command angle at a
    /// fixed probe speed)`. Every peer flying an assembly must build the *same* program
    /// or their thrust commands stand apart for the whole burn, and this pair is how
    /// that is measured (the `BS_BURN_TRACE` `[burn]` line prints it). The probe speed
    /// is fixed on purpose: it compares the two tables directly, rather than each
    /// through its own live speed, which is the only way to tell a genuinely different
    /// program from the same program read at a slightly different point.
    pub fn probe(&self) -> (usize, f32) {
        /// Mid-ascent, comfortably inside the sampled range of any real launch.
        const PROBE_SPEED: f32 = 1000.0;
        (self.samples.len(), self.angle_at(PROBE_SPEED))
    }

    /// The commanded thrust direction at a true position + speed: radial-up tipped by
    /// [`Self::angle_at`] toward [`DOWNRANGE_AZIMUTH`].
    pub fn thrust_dir(&self, true_pos: Vec3, speed: f32) -> Vec3 {
        let up = (true_pos - PLANET_CENTER).normalize_or(Vec3::Y);
        tilt_downrange(up, self.angle_at(speed))
    }
}

/// Speed margin (fraction of escape velocity) that sets the *upper* edge of the cutoff
/// hysteresis band. Escape is `E ≥ 0` (`v = v_esc`); the live autopilot only cuts once a
/// few % faster. The lower edge is plain escape (`E ≥ 0`, outbound), so once cut the engine
/// re-fires only when the ship is *clearly* bound again — energy back below escape or
/// actually falling inbound. Without the band a bare `E ≥ 0` test chatters at the knife
/// edge under prediction/rebase noise and the predicted engine flickers against the
/// server's steady state. Live-control only: the fuel-optimal planner still prices escape
/// at exactly `E ≥ 0`.
pub const ESCAPE_CUTOFF_MARGIN: f32 = 1.06;

/// Whether the live autopilot should hold thrust cut, **with hysteresis** — NOT a one-way
/// latch. `cut` is the current cutoff state, persisted by the caller across ticks: cut
/// *off* (return `true`) once a margin past escape and outbound; turn thrust back *on*
/// (return `false`) the instant escape is genuinely lost (energy below `E ≥ 0`, or radial
/// velocity gone inbound — i.e. the ship is falling back). Between those two thresholds the
/// state is held, so boundary noise can't re-trigger it while a real fall-back still
/// re-fires the engine. Callers clear `cut` when the launch ends so a re-launch thrusts.
pub fn escape_cutoff(true_pos: Vec3, true_vel: Vec3, cut: &mut bool) -> bool {
    *cut = if *cut {
        // Stay cut while escape is still secured (E ≥ 0, outbound); re-fire the instant
        // it's lost — the same canonical cutoff invariant the planner uses.
        escape_secured(true_pos, true_vel)
    } else {
        // Cut once a margin past escape, outbound — clear of the E ≈ 0 knife-edge.
        let r = (true_pos - PLANET_CENTER).length().max(1.0);
        let outbound = (true_pos - PLANET_CENTER).dot(true_vel) >= 0.0;
        let margin_energy = (ESCAPE_CUTOFF_MARGIN * ESCAPE_CUTOFF_MARGIN - 1.0) * GRAVITY_MU / r;
        specific_energy(true_pos, true_vel) >= margin_energy && outbound
    };
    *cut
}

/// The speed (m/s) at which [`escape_cutoff`]'s upper hysteresis edge trips at a given
/// true position: [`ESCAPE_CUTOFF_MARGIN`] times the local escape speed `√(2μ/r)`
/// (substituting `v = M·v_esc` into the margin-energy test makes it exact, outbound).
/// This is the flight HUD's "target speed for engine shutdown" readout — derived here,
/// next to the cutoff it mirrors, so the displayed target can't drift from the real cut.
pub fn cutoff_speed(true_pos: Vec3) -> f32 {
    let r = (true_pos - PLANET_CENTER).length().max(1.0);
    ESCAPE_CUTOFF_MARGIN * (2.0 * GRAVITY_MU / r).sqrt()
}

/// The live autopilot's guidance command: the pitch-program direction at the vehicle's
/// current speed, plus a throttle that cuts to zero once escape is secured (see
/// [`escape_cutoff`] — burning past escape only wastes fuel), with hysteresis so the cut
/// can't flicker at the boundary yet still re-fires if the ship falls back. This is what
/// the three thrust sites fly; [`ascent_thrust_dir`] (the raw closed-loop law) remains the
/// *planning* model the program is sampled from.
pub fn program_guidance(
    true_pos: Vec3,
    true_vel: Vec3,
    program: &PitchProgram,
    cut: &mut bool,
) -> Guidance {
    let throttle = if escape_cutoff(true_pos, true_vel, cut) { 0.0 } else { 1.0 };
    Guidance { thrust_dir: program.thrust_dir(true_pos, true_vel.length()), throttle }
}

/// The outcome of forward-simulating the ascent law as a point mass (see [`propagate`]).
pub struct Prediction {
    /// Sampled true-world positions along the predicted path (for the trajectory line).
    pub path: Vec<Vec3>,
    /// Fuel spent to escape, as impulse per unit mass (m/s) — multiply by the assembly
    /// mass for N·s. `None` if the trajectory crashed or ran out of steps first.
    pub burn_dv: Option<f32>,
    /// Whether escape energy was reached before the budget ran out.
    pub escaped: bool,
}

/// Forward-integrate an ascent as a **point mass** from a true position + velocity under
/// an arbitrary guidance law: thrust of magnitude `thrust_accel × throttle` (m/s²) along
/// the commanded direction, plus the planet's radial gravity and drag, until the law cuts
/// the throttle, a crash below `floor_radius`, or `max_steps`. Semi-implicit Euler at
/// `dt`; the path is sampled every `sample_every` steps.
///
/// The law is a parameter because the two consumers genuinely fly different ones, and
/// which one you integrate is *visible*: the optimizer ranks angles under the ideal
/// closed-loop law ([`propagate`]), while the trajectory preview must fly the open-loop
/// [`PitchProgram`] the autopilot is actually holding ([`propagate_program`]).
///
/// It treats the stack as a point that thrusts exactly along the commanded direction —
/// i.e. it assumes attitude tracks instantly. That's the right fidelity for "where is the
/// autopilot taking me" and for ranking pitchover angles; the live controller handles the
/// real attitude lag (and the optimizer's safety backoff covers the difference).
#[allow(clippy::too_many_arguments)]
pub fn propagate_with(
    mut pos: Vec3,
    mut vel: Vec3,
    vehicle: Vehicle,
    dt: f32,
    max_steps: usize,
    sample_every: usize,
    floor_radius: f32,
    mut command: impl FnMut(Vec3, Vec3) -> Guidance,
) -> Prediction {
    let mut path = Vec::with_capacity(max_steps / sample_every.max(1) + 2);
    let mut burn_dv = 0.0f32;
    // The floor only arms once the trajectory has climbed above it — a floor with a
    // clearance margin sits ABOVE the launch pad, and enforcing it from step 0 would
    // declare every launch crashed at lift-off. Below the arm point the true ground
    // still applies.
    let mut cleared_floor = false;
    for step in 0..max_steps {
        let Guidance { thrust_dir, throttle } = command(pos, vel);
        // The law cut the engines: the burn is over, which is exactly the end of the
        // trajectory these callers want (the coast beyond it is a separate preview).
        // Queried before the sample push so the cut point is recorded once, not twice.
        if throttle <= 0.0 {
            path.push(pos);
            return Prediction { path, burn_dv: Some(burn_dv), escaped: true };
        }
        if step % sample_every.max(1) == 0 {
            path.push(pos);
        }
        // Drag brakes the climb (mass-independent force ÷ mass = deceleration); the burn
        // has to fight through it, so a draggier ascent naturally takes more steps and
        // more impulse to reach escape (`burn_dv` counts only thrust, as fuel should).
        let accel = thrust_dir * (vehicle.thrust_accel * throttle)
            + gravity_at(pos)
            + crate::map::drag_force(pos, vel) / vehicle.mass;
        burn_dv += vehicle.thrust_accel * throttle * dt;
        vel += accel * dt;
        pos += vel * dt;
        // Sank below the (armed) floor before escaping → crashed; fuel-to-escape is
        // undefined.
        let r = (pos - PLANET_CENTER).length();
        if r >= floor_radius {
            cleared_floor = true;
        }
        if r < if cleared_floor { floor_radius } else { GROUND_RADIUS } {
            path.push(pos);
            return Prediction { path, burn_dv: None, escaped: false };
        }
    }
    Prediction { path, burn_dv: None, escaped: false }
}

/// Forward-integrate the **ideal** ascent law ([`ascent_thrust_dir`]) at full throttle,
/// terminating the instant escape is secured. This is the planning model: the optimizer
/// ranks pitchover angles by the `burn_dv` it reports, and it prices escape at exactly
/// `E ≥ 0` — no live hysteresis margin, which is a control detail, not a fuel cost.
#[allow(clippy::too_many_arguments)]
pub fn propagate(
    pos: Vec3,
    vel: Vec3,
    vehicle: Vehicle,
    pitchover: f32,
    dt: f32,
    max_steps: usize,
    sample_every: usize,
    floor_radius: f32,
) -> Prediction {
    propagate_with(pos, vel, vehicle, dt, max_steps, sample_every, floor_radius, |p, v| {
        Guidance {
            thrust_dir: ascent_thrust_dir(p, v, pitchover),
            throttle: if escape_secured(p, v) { 0.0 } else { 1.0 },
        }
    })
}

/// Forward-integrate the ascent the autopilot is **actually flying**: the open-loop
/// [`PitchProgram`] plus its live escape cutoff (hysteresis margin included), from a
/// mid-flight state. This is what the trajectory line draws.
///
/// It is deliberately NOT [`propagate`]. The ideal law steers *prograde* above
/// [`TURN_SPEED`], so re-running it from the live state bakes the vehicle's current
/// velocity **direction** into the forecast and integrates that for the whole remaining
/// burn — every deviation of the real stack from the ideal arc (attitude lag, a realized
/// thrust above the derated plan) swings the far end of the line and re-draws it somewhere
/// new on the next replan. Flying the program forecasts the same command schedule the
/// rocket is holding, so the drawn path only moves as the vehicle's own state moves.
/// Measured on a TWR-1.3 stack with 1 s of attitude lag: the ideal law wandered its
/// predicted cutoff point over ~1 km and jumped 45–61 m per half-second replan through the
/// early climb (~2.5 km and 550 m with the derated-thrust mismatch as well); the program
/// forecast moved ~1 m per replan.
#[allow(clippy::too_many_arguments)]
pub fn propagate_program(
    pos: Vec3,
    vel: Vec3,
    vehicle: Vehicle,
    program: &PitchProgram,
    dt: f32,
    max_steps: usize,
    sample_every: usize,
    floor_radius: f32,
) -> Prediction {
    // The cutoff is hysteretic, so it carries state across the forecast's steps exactly
    // as the live autopilot's does across ticks — starting `false` because a preview is
    // only ever drawn while the engines are still burning.
    let mut cut = false;
    propagate_with(pos, vel, vehicle, dt, max_steps, sample_every, floor_radius, move |p, v| {
        program_guidance(p, v, program, &mut cut)
    })
}

/// Point-mass integration step for [`propagate`] / [`optimize_pitchover`] (s). Coarse is
/// fine: the sweep only *ranks* pitchover angles, it doesn't fly them.
pub const OPTIMIZER_DT: f32 = 0.1;
/// Step budget for one optimizer trajectory — 0.1 s × 4000 = 400 sim-seconds, enough for
/// the slowest barely-lifting stack to reach escape or demonstrably fail.
pub const OPTIMIZER_STEPS: usize = 4000;

/// Find the pitchover angle (rad) that reaches escape on the least fuel from a given true
/// state, by forward-simulating [`propagate`] over a coarse-then-refined sweep of angles.
/// The [`Vehicle`] carries the assembly's full-throttle thrust acceleration (Σ engine
/// thrust / total mass) and its total mass (for the drag deceleration). This is the
/// per-assembly "figure out the efficient path" step — it adapts the flight plan to
/// whatever the player built *and* to the air it must climb through.
///
/// The min-fuel angle sits right at the crash boundary (more lean = less gravity loss,
/// until the arc clips the terrain), and the real attitude-lagged vehicle shouldn't fly
/// the exact boundary — so the returned angle is backed off by `SAFETY`, trading a little
/// fuel for altitude margin.
pub fn optimize_pitchover(true_pos: Vec3, true_vel: Vec3, vehicle: Vehicle) -> f32 {
    const SAFETY: f32 = 0.85;
    /// Attitude-execution cost of a commanded lean, as extra fuel Δv (m/s) per radian of
    /// pitchover. The point-mass sim assumes thrust snaps to the guidance direction; the
    /// real stack must physically rotate, and imperfect tracking points some thrust
    /// off-plan. Charging the sweep for it keeps the optimizer off angles that only pay
    /// on paper, and gives engine-dense stacks (steep ideal turn, small gravity-loss
    /// prize) their measured-cheapest near-vertical ascent.
    ///
    /// Calibration history — this constant is tied to HOW the command is flown:
    /// - Prograde-chasing era (thrust tracked the live velocity vector): execution ate
    ///   5–7% of every burn at ANY angle (allocator trimmed continuously against the
    ///   moving target), and a TWR-2.1 stack commanded 29° trimmed itself to
    ///   hover-thrust and broke up → the penalty had to sit at 300 m/s/rad, which
    ///   capped every build's turn at ~17°.
    /// - Pitch-program era (current): the command is a smooth angle-vs-speed schedule,
    ///   as cheap to hold as vertical — measured turn savings now land ON the
    ///   point-mass prediction (19–22%). 50 covers the residual attitude lag while
    ///   letting mid-TWR builds fly the real optimum (~25–35°); the straight-up
    ///   transition moves to TWR ≈ 2.2–2.4, which engine-count asymptotics make
    ///   genuinely "lots of rockets" territory (a bare engine is TWR ~2.2).
    const EXECUTION_PENALTY_PER_RAD: f32 = 50.0;
    let cost = |angle: f32| {
        let ideal = propagate(
            true_pos,
            true_vel,
            vehicle,
            angle,
            OPTIMIZER_DT,
            OPTIMIZER_STEPS,
            OPTIMIZER_STEPS, // no path samples needed — only the fuel figure
            GRAVITY_REF_RADIUS + OPTIMIZER_CLEARANCE_M,
        )
        .burn_dv
        .unwrap_or(f32::INFINITY);
        ideal + EXECUTION_PENALTY_PER_RAD * angle
    };
    // Coarse sweep 0..75°, then ternary refinement around the best — the cost is smooth
    // and unimodal in the angle (rising gravity loss toward 0°, the crash cliff toward
    // horizontal), and INFINITY past the cliff.
    let mut best_angle = 0.0f32;
    let mut best_cost = f32::INFINITY;
    let coarse = 15;
    for i in 0..=coarse {
        let angle = (i as f32 / coarse as f32) * 75.0_f32.to_radians();
        let c = cost(angle);
        if c < best_cost {
            best_cost = c;
            best_angle = angle;
        }
    }
    let mut lo = (best_angle - 5.0_f32.to_radians()).max(0.0);
    let mut hi = best_angle + 5.0_f32.to_radians();
    for _ in 0..12 {
        let m1 = lo + (hi - lo) / 3.0;
        let m2 = hi - (hi - lo) / 3.0;
        if cost(m1) < cost(m2) {
            hi = m2;
        } else {
            lo = m1;
        }
    }
    // The refinement may only *improve* on the coarse winner. Ternary search needs a
    // strictly unimodal cost; this one is `INFINITY` for every angle whose arc crashes,
    // and `INFINITY < INFINITY` is false — so a bracket whose interior is entirely
    // infeasible drives `lo` up to `hi` on every iteration and returns the top edge, an
    // angle the planner itself scored as a crash. Measured: a TWR-1.14 stack (two riders
    // on a four-engine build), where 0° is the ONLY feasible angle on the whole sweep,
    // was handed 4.23° — and flew it into the ground at ~900 m where vertical reached
    // 40 km. Falling back to the coarse best also gives the right answer when *nothing*
    // is feasible: `best_angle` is still its 0.0 seed, i.e. straight up.
    let refined = 0.5 * (lo + hi);
    if cost(refined) < best_cost { refined * SAFETY } else { best_angle * SAFETY }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::SURFACE_GRAVITY;

    /// Replay the *flown* command — the recorded table, sampled open-loop against speed —
    /// through the same point-mass dynamics the planner uses. Returns `(peak radial
    /// altitude, escaped)`.
    fn replay(
        program: &PitchProgram,
        mut pos: Vec3,
        mut vel: Vec3,
        vehicle: Vehicle,
    ) -> (f32, bool) {
        let mut peak = f32::MIN;
        for _ in 0..OPTIMIZER_STEPS {
            peak = peak.max(crate::map::radial_altitude(pos));
            let dir = program.thrust_dir(pos, vel.length());
            let accel = dir * vehicle.thrust_accel
                + gravity_at(pos)
                + crate::map::drag_force(pos, vel) / vehicle.mass;
            vel += accel * OPTIMIZER_DT;
            pos += vel * OPTIMIZER_DT;
            if (pos - PLANET_CENTER).length() < GROUND_RADIUS {
                return (peak, false);
            }
            if escape_secured(pos, vel) {
                return (peak, true);
            }
        }
        (peak, false)
    }

    /// The optimizer must never hand back an angle its own model scores as a crash.
    ///
    /// Regression for the ternary refinement: the sweep's cost is `INFINITY` for any angle
    /// whose arc crashes, and `INFINITY < INFINITY` is false, so a bracket with an entirely
    /// infeasible interior walked `lo` up to `hi` and returned the top edge. A TWR-1.14
    /// stack (two riders aboard a four-engine build), for which 0° is the ONLY feasible
    /// angle, was handed 4.23° — and flew it into the ground at ~900 m where vertical
    /// reached 40 km. The low end of this sweep is that vehicle.
    #[test]
    fn optimizer_never_returns_an_arc_it_scores_as_a_crash() {
        let mass = 17.907f32;
        let (pos, vel) = (Vec3::new(0.0, 1.0, 0.0), Vec3::ZERO);
        for twr in [1.05f32, 1.10, 1.14, 1.20, 1.30, 1.50, 2.00] {
            let vehicle = Vehicle { thrust_accel: twr * SURFACE_GRAVITY, mass };
            let chosen = optimize_pitchover(pos, vel, vehicle);
            let flies = propagate(
                pos, vel, vehicle, chosen, OPTIMIZER_DT, OPTIMIZER_STEPS, OPTIMIZER_STEPS,
                GRAVITY_REF_RADIUS + OPTIMIZER_CLEARANCE_M,
            )
            .burn_dv
            .is_some();
            // Vertical is the sanctioned fallback: a stack too weak to escape at any angle
            // should climb, not lean into a turn the planner already priced as fatal.
            assert!(
                flies || chosen == 0.0,
                "TWR {twr:.2}: chose {:.2}°, which the planner itself scores as a crash",
                chosen.to_degrees(),
            );
        }
    }

    /// The flown command must reproduce the arc the optimizer priced.
    ///
    /// The optimizer ranks angles with `propagate`, which recomputes the ideal law from the
    /// live state every step (closed loop); the autopilot instead replays [`PitchProgram`]'s
    /// table indexed by speed (open loop). Those are only interchangeable while the table
    /// round-trips — if it stops, the planner is optimizing a trajectory nothing flies.
    #[test]
    fn flown_table_reproduces_the_planned_arc() {
        let vehicle = Vehicle { thrust_accel: 11.1760, mass: 17.907 };
        let (pos, vel) = (Vec3::new(0.0, 1.0, 0.0), Vec3::ZERO);
        for deg in [0.0f32, 1.0, 2.0, 4.0] {
            let angle = deg.to_radians();
            let planned = propagate(
                pos, vel, vehicle, angle, OPTIMIZER_DT, OPTIMIZER_STEPS, 1,
                GRAVITY_REF_RADIUS + OPTIMIZER_CLEARANCE_M,
            );
            let peak_planned = planned
                .path
                .iter()
                .map(|p| crate::map::radial_altitude(*p))
                .fold(f32::MIN, f32::max);
            let program = PitchProgram::build(LaunchSeed {
                pitchover: angle,
                position: pos.to_array(),
                velocity: vel.to_array(),
                vehicle,
            });
            let (peak_flown, _) = replay(&program, pos, vel, vehicle);
            assert!(
                (peak_flown - peak_planned).abs() < 0.05 * peak_planned.abs(),
                "{deg}°: flown table peaks at {peak_flown:.0} m but the plan priced \
                 {peak_planned:.0} m",
            );
        }
    }

    /// Fly `program` the way the real stack does — with first-order attitude lag, at a
    /// realized thrust above the derated value it was planned against — and return the
    /// true state every half second of the climb. Both of those are permanent facts about
    /// the vehicle, not tuning: the plan is deliberately derated to the allocator's lift
    /// floor, and a physical stack rotates toward a command rather than snapping to it.
    fn lagged_flight(program: &PitchProgram, planned: Vehicle) -> Vec<(Vec3, Vec3)> {
        const ATTITUDE_LAG_SECS: f32 = 1.0;
        let real_thrust = planned.thrust_accel / crate::launch::LIFT_FLOOR;
        let (mut pos, mut vel) = (Vec3::from_array(program.seed.position), Vec3::ZERO);
        let (mut aim, mut cut, mut states) = (Vec3::Y, false, Vec::new());
        for i in 0..OPTIMIZER_STEPS {
            let g = program_guidance(pos, vel, program, &mut cut);
            if g.throttle == 0.0 {
                break;
            }
            if i % 5 == 0 {
                states.push((pos, vel));
            }
            aim = (aim + (g.thrust_dir - aim) * (OPTIMIZER_DT / ATTITUDE_LAG_SECS)).normalize();
            let accel = aim * real_thrust
                + gravity_at(pos)
                + crate::map::drag_force(pos, vel) / planned.mass;
            vel += accel * OPTIMIZER_DT;
            pos += vel * OPTIMIZER_DT;
        }
        states
    }

    /// The largest step the forecast's end point takes between consecutive replans — i.e.
    /// how much the drawn trajectory jumps ahead of the rocket while you watch it.
    fn worst_replan_jump(states: &[(Vec3, Vec3)], forecast: impl Fn(Vec3, Vec3) -> Vec3) -> f32 {
        states
            .windows(2)
            .map(|w| forecast(w[0].0, w[0].1).distance(forecast(w[1].0, w[1].1)))
            .fold(0.0, f32::max)
    }

    /// The trajectory line must forecast the program the autopilot is **holding**, not the
    /// ideal law the optimizer ranked angles with.
    ///
    /// Regression for a visibly wandering flight line: the preview re-ran [`propagate`]
    /// from the live state every half second, and that law steers *prograde* above
    /// [`TURN_SPEED`] — so it baked the vehicle's current velocity direction into the
    /// forecast and integrated it over the whole remaining burn. Because a real stack
    /// deviates from the ideal arc (attitude lag; realized thrust above the derated plan),
    /// the drawn end point swung by kilometres and landed somewhere new on every replan.
    /// [`propagate_program`] forecasts the command schedule actually being flown, so it
    /// moves only as the vehicle's own state does.
    #[test]
    fn the_drawn_forecast_holds_still_while_the_ideal_law_wanders() {
        let g = Vec3::new(0.0, -SURFACE_GRAVITY, 0.0);
        let (engines, mass) = (4usize, 15.5f32);
        let start = Vec3::new(0.0, 1.0, 0.0);
        let planned = Vehicle::derated(engines, g, mass);
        let program = PitchProgram::plan(start, Vec3::ZERO, engines, g, mass, None);
        let states = lagged_flight(&program, planned);
        assert!(states.len() > 20, "need a real climb to watch: {} samples", states.len());

        let end = |p: Prediction| *p.path.last().expect("a non-empty path");
        let flown = worst_replan_jump(&states, |pos, vel| {
            end(propagate_program(
                pos, vel, planned, &program, OPTIMIZER_DT, OPTIMIZER_STEPS, 5, GROUND_RADIUS,
            ))
        });
        let ideal = worst_replan_jump(&states, |pos, vel| {
            end(propagate(
                pos, vel, planned, program.seed.pitchover, OPTIMIZER_DT, OPTIMIZER_STEPS, 5,
                GROUND_RADIUS,
            ))
        });
        // The forecast is allowed to track the vehicle — it just can't leap. The bound is
        // loose enough to be about the law and not the integrator, and the old behaviour
        // misses it by a wide margin (measured: ~550 m).
        assert!(
            flown < 100.0,
            "the flown-program forecast jumped {flown:.0} m between replans",
        );
        // The contrast is the point: if the two laws ever forecast a lagged flight equally
        // well, this test has stopped demonstrating why the preview must fly the program
        // (the client-side twin, `trajectory::the_line_ahead_does_not_leap_on_a_wobble`,
        // is what pins the call site to it).
        assert!(
            ideal > 3.0 * flown,
            "the ideal law was supposed to be the unstable one, but it jumped {ideal:.0} m \
             against the program's {flown:.0} m",
        );
    }

    /// A point on the pad, at rest, is deep in the well: energy is very negative.
    #[test]
    fn at_rest_on_pad_is_bound() {
        let pos = Vec3::new(0.0, 0.0, 0.0); // world y=0 = the pad, r = GRAVITY_REF_RADIUS
        assert!(!escaped(pos, Vec3::ZERO));
        assert!(specific_energy(pos, Vec3::ZERO) < 0.0);
    }

    /// Escape speed at the surface is √(2μ/R); just over it escapes, just under doesn't —
    /// regardless of direction (energy is a scalar).
    #[test]
    fn escape_speed_threshold() {
        let pos = Vec3::new(0.0, 0.0, 0.0);
        let v_esc = (2.0 * GRAVITY_MU / GRAVITY_REF_RADIUS).sqrt();
        assert!((v_esc - (2.0 * SURFACE_GRAVITY * GRAVITY_REF_RADIUS).sqrt()).abs() < 1.0);
        for dir in [Vec3::Y, Vec3::X, Vec3::new(1.0, 1.0, 0.0).normalize()] {
            assert!(escaped(pos, dir * (v_esc + 1.0)), "should escape along {dir:?}");
            assert!(!escaped(pos, dir * (v_esc - 1.0)), "should not escape along {dir:?}");
        }
    }

    /// Zero pitchover commands radial-out regardless of drift — it never chases a
    /// disturbance; a nonzero pitchover leans at low speed and goes prograde once fast.
    #[test]
    fn steering_vertical_kick_then_prograde() {
        let pos = Vec3::new(0.0, 100.0, 0.0);
        let up = (pos - PLANET_CENTER).normalize();
        for vel in [Vec3::ZERO, Vec3::new(50.0, 5.0, 0.0)] {
            let dir = ascent_thrust_dir(pos, vel, 0.0);
            assert!(dir.dot(up) > 0.999, "zero pitchover should be radial-up, got {dir:?}");
        }
        let slow = ascent_thrust_dir(pos, Vec3::new(0.0, 1.0, 0.0), 10.0_f32.to_radians());
        assert!(slow.dot(Vec3::Y) > 0.9 && slow.x > 0.0, "kick should tip toward +x: {slow:?}");
        let vel = Vec3::new(200.0, 200.0, 0.0);
        let fast = ascent_thrust_dir(pos, vel, 10.0_f32.to_radians());
        assert!(fast.dot(vel.normalize()) > 0.999, "fast should be prograde: {fast:?}");
    }

    /// Guidance cuts a margin past escape with hysteresis: boundary noise can't re-ignite
    /// it, but a genuine fall-back does.
    #[test]
    fn throttle_cutoff_has_hysteresis() {
        let pos = Vec3::new(0.0, 0.0, 0.0); // one ref-radius from the planet centre
        let v_esc = (2.0 * GRAVITY_MU / GRAVITY_REF_RADIUS).sqrt();
        let program = PitchProgram::default();
        let mut cut = false;
        // Barely past escape but inside the cutoff margin, outbound: still burning.
        assert_eq!(
            program_guidance(pos, Vec3::Y * (v_esc + 5.0), &program, &mut cut).throttle,
            1.0
        );
        assert!(!cut);
        // Clearly past the margin, outbound: cut.
        assert_eq!(
            program_guidance(pos, Vec3::Y * v_esc * 1.1, &program, &mut cut).throttle,
            0.0
        );
        assert!(cut);
        // In the dead-band (just above escape, outbound): stays cut — no flicker.
        assert_eq!(
            program_guidance(pos, Vec3::Y * (v_esc + 5.0), &program, &mut cut).throttle,
            0.0
        );
        // Genuinely bound again (below escape energy): the engine re-fires — not latched.
        assert_eq!(
            program_guidance(pos, Vec3::Y * 10.0, &program, &mut cut).throttle,
            1.0
        );
        assert!(!cut);
        // Past the margin but falling inbound (negative radial vel): does NOT cut.
        let mut cut2 = false;
        assert_eq!(
            program_guidance(pos, Vec3::NEG_Y * v_esc * 1.2, &program, &mut cut2).throttle,
            1.0
        );
    }

    /// `cutoff_speed` is exactly where the cutoff's upper edge trips: just under it the
    /// engine still burns, just over it (outbound) it cuts — so the HUD target it feeds
    /// can't disagree with the real shutdown.
    #[test]
    fn cutoff_speed_matches_the_cutoff_edge() {
        let pos = Vec3::new(0.0, 0.0, 0.0);
        let target = cutoff_speed(pos);
        let mut below = false;
        assert!(!escape_cutoff(pos, Vec3::Y * (target - 1.0), &mut below));
        let mut above = false;
        assert!(escape_cutoff(pos, Vec3::Y * (target + 1.0), &mut above));
    }

    /// The shared plan constructor guards a mass-less assembly with the straight-up
    /// default (a zero-thrust "plan" would sample a garbage gravity-only fall), and
    /// embeds a forced angle verbatim (the replicated-rebuild and A/B-override path).
    #[test]
    fn plan_guards_zero_mass_and_honors_forced_angle() {
        let g = Vec3::new(0.0, -SURFACE_GRAVITY, 0.0);
        let empty = PitchProgram::plan(Vec3::ZERO, Vec3::ZERO, 4, g, 0.0, Some(0.3));
        assert_eq!(empty.seed.pitchover, 0.0);
        assert_eq!(empty.angle_at(100.0), 0.0);
        let forced = PitchProgram::plan(Vec3::ZERO, Vec3::ZERO, 4, g, 12.0, Some(0.3));
        assert_eq!(forced.seed.pitchover, 0.3);
        assert!(forced.angle_at(TURN_SPEED) > 0.2, "kick should approach the forced angle");
    }

    /// The optimizer never picks a crashing angle, and its choice beats or matches
    /// straight up on fuel — with a low-TWR stack it must beat it by a wide margin
    /// (that's the whole point of the turn at real-rocket thrust levels).
    #[test]
    fn optimizer_beats_straight_up_at_low_twr() {
        let pos = Vec3::new(0.0, 0.0, 0.0);
        // A heavy hauler; drag-negligible mass so this isolates the gravity-turn benefit.
        let vehicle = Vehicle { thrust_accel: 1.35 * SURFACE_GRAVITY, mass: 1.0e6 };
        let straight = propagate(
            pos,
            Vec3::ZERO,
            vehicle,
            0.0,
            OPTIMIZER_DT,
            OPTIMIZER_STEPS,
            OPTIMIZER_STEPS,
            GROUND_RADIUS,
        );
        assert!(straight.escaped, "even TWR 1.35 escapes straight up eventually");
        let angle = optimize_pitchover(pos, Vec3::ZERO, vehicle);
        assert!(angle > 1.0_f32.to_radians(), "low TWR should get a real lean: {angle}");
        let turned = propagate(
            pos,
            Vec3::ZERO,
            vehicle,
            angle,
            OPTIMIZER_DT,
            OPTIMIZER_STEPS,
            OPTIMIZER_STEPS,
            GROUND_RADIUS,
        );
        let (s, t) = (straight.burn_dv.unwrap(), turned.burn_dv.unwrap());
        assert!(t < s * 0.9, "turn ({t:.0}) should save >10% vs straight ({s:.0})");
    }

    /// Drag makes the planning sim cost more fuel to escape — the point-mass model the
    /// autopilot plans against is genuinely drag-aware (same stack, thinner-vs-thicker
    /// effective air via a light-vs-heavy mass on the shared drag force).
    #[test]
    fn drag_costs_extra_fuel_in_the_plan() {
        let pos = Vec3::new(0.0, 0.0, 0.0);
        let thrust_accel = 3.0 * SURFACE_GRAVITY; // escapes straight up in either case
        let run = |mass: f32| {
            propagate(
                pos,
                Vec3::ZERO,
                Vehicle { thrust_accel, mass },
                0.0,
                OPTIMIZER_DT,
                OPTIMIZER_STEPS,
                OPTIMIZER_STEPS,
                GROUND_RADIUS,
            )
        };
        let clean = run(1.0e6); // drag-negligible
        let draggy = run(40.0); // a light stack the air actually brakes
        assert!(clean.escaped && draggy.escaped, "both reach escape");
        assert!(
            draggy.burn_dv.unwrap() > clean.burn_dv.unwrap() * 1.02,
            "drag should cost measurably more fuel: clean {:.0} vs draggy {:.0}",
            clean.burn_dv.unwrap(),
            draggy.burn_dv.unwrap()
        );
    }

    /// The trajectory prediction produces a path that climbs and ends escaped.
    #[test]
    fn predicted_path_climbs_and_escapes() {
        let pos = Vec3::new(0.0, 0.0, 0.0);
        let p = propagate(
            pos,
            Vec3::ZERO,
            Vehicle { thrust_accel: 2.0 * SURFACE_GRAVITY, mass: 1.0e6 },
            10.0_f32.to_radians(),
            OPTIMIZER_DT,
            OPTIMIZER_STEPS,
            20,
            GROUND_RADIUS,
        );
        assert!(p.escaped && p.path.len() > 5);
        let climb = (*p.path.last().unwrap() - PLANET_CENTER).length()
            - (pos - PLANET_CENTER).length();
        assert!(climb > 1000.0, "path should climb well clear of the pad: {climb:.0} m");
    }
}


