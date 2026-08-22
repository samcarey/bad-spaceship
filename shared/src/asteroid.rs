//! The asteroid field a launched ship flies through — the difficulty curve and the
//! geometry of where each rock comes from.
//!
//! Pure functions over the assembly's flight state, exactly like [`crate::guidance`] and
//! [`crate::launch`]: no ECS, no lightyear, no randomness of its own. The server draws the
//! random numbers and spawns the bodies; everything about *what* to spawn is decided here
//! so the curve can be read, tuned, and tested in one place.
//!
//! **The frame — and it is the ship's, not the world's.** Rocks are placed *and given their
//! velocity* relative to the ship, in the pilot's own
//! [`flight_frame`](crate::guidance::flight_frame): ahead along the direction of flight,
//! scattered across it, closing at a chosen relative speed.
//!
//! This is the load-bearing decision. An ascent's speed climbs through three orders of
//! magnitude and its heading swings from vertical to horizontal, so a field defined against
//! the world is unreachable at one end of the climb and unavoidable at the other. Defined
//! against the ship, one curve gives the same encounter — the same warning time, the same
//! crossing angle, the same deflection needed to miss — from the pad to escape velocity.
//!
//! Getting this half-right is worse than either: the first cut here placed rocks in the
//! flight frame but gave them world velocities, aimed at where the ship *was*. By arrival
//! the ship had moved `speed × `[`WARNING_SECS`], so at 400 m/s every rock missed by 1.4 km
//! and the field was scenery — 131 rocks in a measured flight, not one of them a threat.
//! The velocity has to be relative for the same reason the placement does.
//!
//! What that buys costs something honest: the rubble is co-moving, drifting through the
//! ship's path rather than tearing past it at orbital speed. Everything downstream of the
//! spawn is still ordinary physics — the planet's gravity pulls each rock down, they
//! collide with the stack and with each other, and the ship's thrust pulls it off the
//! intercept — so what the pilot flies through behaves like rubble in freefall, not like a
//! scripted obstacle course.
//!
//! **The difficulty.** One scalar, [`difficulty`], ramps from 0 to 1 over
//! [`RAMP_SECS`] of flight and every knob reads off it: rocks arrive more often, grow
//! bigger, close faster, and — the part that actually makes it hard — scatter into a
//! *tighter* cone around the ship's path, so late in the climb they must be dodged rather
//! than merely watched. Because size grows while the ship's mass does not, the threat
//! changes in kind as well as degree: an early rock is a shove, a late one ends the flight.

use crate::guidance::flight_frame;
use bevy::math::Vec3;

/// Seconds after blastoff before the first rock appears — long enough to clear the pad and
/// settle onto the program with both hands free.
pub const FIRST_ROCK_SECS: f32 = 5.0;

/// Seconds of flight over which [`difficulty`] ramps 0 → 1, measured from
/// [`FIRST_ROCK_SECS`].
///
/// Sized against a *measured* ascent rather than a guess: the reference four-engine stack
/// reaches its escape cutoff about 60 s after blastoff. At the first cut's 100 s ramp a
/// whole flight was over before the field passed half intensity, so the escalation the
/// curve exists to produce simply never arrived. Sixty seconds puts the hardest rocks
/// against the last of the burn.
pub const RAMP_SECS: f32 = 60.0;

/// Mean seconds between rocks at zero difficulty and at full — the field goes from a
/// straggler every few seconds to a stream.
pub const INTERVAL_EASY: f32 = 3.0;
pub const INTERVAL_HARD: f32 = 0.45;

/// Rock radius band (m) at zero difficulty and at full. Even the small end is big against
/// a launch stack (parts are ~1 m); the large end is a hill.
pub const RADIUS_EASY: (f32, f32) = (2.5, 5.0);
pub const RADIUS_HARD: (f32, f32) = (5.0, 10.0);

/// Density (kg/m³ in this sim's units) of a rock — a porous rubble pile, well under the
/// [`crate::part::PART_DENSITY`] of the ship's own parts.
///
/// This number is the difficulty curve's teeth. Mass goes as `r³`, so across the radius
/// band above a rock runs from about the mass of a few parts to a hundred times the whole
/// stack: an early rock is a survivable shove that knocks the ship off its program, and a
/// late one is a wall. Nothing else in the field needs to escalate the *consequence* — the
/// consequence is what a sphere of that size does when it arrives.
pub const ROCK_DENSITY: f32 = 0.6;

/// Closing speed band (m/s, relative to the ship) at zero difficulty and at full.
pub const CLOSING_EASY: (f32, f32) = (70.0, 110.0);
pub const CLOSING_HARD: (f32, f32) = (160.0, 260.0);

/// How much warning the pilot gets: rocks are placed this many seconds of *relative*
/// closing ahead, so the time from "visible" to "here" is constant no matter how fast
/// either body is moving. Difficulty is expressed by the *scatter*, not by shortening the
/// fuse — a rock that appears too late to react to isn't difficulty, it's a coin flip.
pub const WARNING_SECS: f32 = 3.5;

/// Radius (m) of the disc rocks are scattered across, at zero difficulty and at full,
/// measured across the flight path at the point they are placed.
///
/// Shrinking this is what turns the field from scenery into a gauntlet: a wide scatter
/// puts most rocks somewhere off to the side, a tight one puts them on the nose.
///
/// The easy end is bounded by *sight*, not by safety. Rocks are placed a fixed fuse ahead,
/// so a scatter much wider than the view frustum at that range puts the early ones off the
/// edge of the screen — a field the pilot never sees coming is neither easy nor hard, it is
/// absent. 130 m at the ~450 m placement range keeps them in frame while still missing by
/// a comfortable margin.
pub const SCATTER_EASY: f32 = 130.0;
pub const SCATTER_HARD: f32 = 55.0;

/// Downward (anti-radial) drift given to every rock on top of its closing velocity, so the
/// field reads as *falling* rather than as oncoming traffic — most visible once the ship
/// has pitched over and the rubble crosses its path from above.
pub const FALL_SPEED: f32 = 55.0;

/// Peak tumble rate (rad/s); each rock gets a random spin up to this.
pub const MAX_TUMBLE: f32 = 1.2;

/// How far from the assembly a rock is allowed to get before it is despawned (m).
/// Comfortably past both the placement distance and any plausible miss.
pub const DESPAWN_RADIUS: f32 = 2_500.0;

/// A rock that is already **receding** is done, and is swept once it is this far behind
/// (m) rather than being carried all the way out to [`DESPAWN_RADIUS`].
///
/// Without this the population is set by the ratio of the despawn radius to the closing
/// speed, which the difficulty curve also moves — so tightening the interval quietly
/// multiplies the live count until it pins against the spawner's cap and the field stops
/// getting harder. Sweeping on "has it passed?" makes the two independent: the curve owns
/// the density, this owns the cleanup.
pub const PAST_RADIUS: f32 = 300.0;

/// Whether a rock has passed the ship and can be swept: either it is beyond
/// [`DESPAWN_RADIUS`] whatever it is doing, or it is receding and already
/// [`PAST_RADIUS`] behind. All four arguments are in one frame; which one does not matter,
/// since only the differences are read.
pub fn rock_is_spent(rock_pos: Vec3, rock_vel: Vec3, ship_pos: Vec3, ship_vel: Vec3) -> bool {
    let offset = rock_pos - ship_pos;
    let range = offset.length();
    range > DESPAWN_RADIUS || (range > PAST_RADIUS && offset.dot(rock_vel - ship_vel) > 0.0)
}

/// The field's intensity `0..=1` at `elapsed` seconds since blastoff: zero until
/// [`FIRST_ROCK_SECS`], then a linear ramp to full over [`RAMP_SECS`], then held.
pub fn difficulty(elapsed: f32) -> f32 {
    ((elapsed - FIRST_ROCK_SECS) / RAMP_SECS).clamp(0.0, 1.0)
}

/// Mean seconds between rocks at intensity `d`.
pub fn spawn_interval(d: f32) -> f32 {
    lerp(INTERVAL_EASY, INTERVAL_HARD, d)
}

/// The most rocks one flight may have in the air at once.
///
/// Not a difficulty knob — the interval and the sweep set the real density between them —
/// but a backstop, so a ship that stops moving (or a field left running by some future
/// bug) can't grow an unbounded replicated entity count for every client in the room to
/// simulate.
pub const MAX_LIVE_ROCKS: usize = 48;

/// A flight's asteroid-field clock: how long it has been under way, and when the next rock
/// is due.
///
/// Shared because the *schedule* is as much a part of the field's feel as the curve is, and
/// there are two owners of it — the server for multiplayer rooms, the client for single
/// player. Splitting the curve into `shared` while leaving each owner to decide when to
/// read it would let the two drift apart in exactly the way that is hardest to notice: same
/// constants, different game.
#[derive(Clone, Copy, Default, Debug)]
pub struct FieldClock {
    /// Seconds since blastoff — the difficulty curve's only input.
    pub elapsed: f32,
    /// The `elapsed` the next rock is due at. Zero before the first is scheduled.
    next_at: f32,
}

impl FieldClock {
    /// Advance by `dt` and report the intensity to spawn at, or `None` if nothing is due.
    ///
    /// Scheduling the first rock lazily (rather than starting at zero) is what keeps the
    /// hold-off from accumulating: a clock that simply compared against
    /// [`FIRST_ROCK_SECS`] would come out of it owing several rocks and fire them all at
    /// once. For the same reason exactly one rock is ever due per call — catching up after
    /// a stall (a hitching server, a suspended tab) would only throw an unfair wall at a
    /// pilot who was not there for it.
    pub fn tick(&mut self, dt: f32) -> Option<f32> {
        self.elapsed += dt;
        if self.next_at == 0.0 {
            self.next_at = FIRST_ROCK_SECS;
        }
        if self.elapsed < self.next_at {
            return None;
        }
        let d = difficulty(self.elapsed);
        self.next_at = self.elapsed + spawn_interval(d) * jitter(self.elapsed);
        Some(d)
    }

    /// Seconds until the next rock — for tracing.
    pub fn until_next(&self) -> f32 {
        self.next_at - self.elapsed
    }
}

/// A spread in `[0.6, 1.4)` that keeps the field from ticking like a metronome, derived
/// from the flight clock itself.
///
/// Deliberately not randomness: its only job is to break the rhythm, and unlike a `rand`
/// draw it needs no generator threaded through the caller and costs nothing to reason
/// about — the fractional part of a clock advancing by an irregular amount every tick is
/// already as uncorrelated as this needs to be.
fn jitter(elapsed: f32) -> f32 {
    0.6 + 0.8 * (elapsed * 7.3).fract().abs()
}

/// Everything the server needs to spawn one rock, in the room's **local** frame — the same
/// frame the assembly's `Position`/`LinearVelocity` are in, so no floating-origin
/// bookkeeping is needed at the spawn site.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RockSpawn {
    pub position: Vec3,
    pub velocity: Vec3,
    pub spin: Vec3,
    pub radius: f32,
    /// Appearance seed, carried on `NetPart::seed` so every client carves the same rock.
    pub seed: u32,
}

/// Place one rock for an assembly at `com` moving at `ship_vel` (both room-local — the
/// frame the returned spawn is expressed in), flying `true_pos`/`true_vel` in the true
/// planet frame (which is what the flight frame and the planet's radial are read from), at
/// intensity `d`.
///
/// The two velocities are both needed and are not interchangeable: `true_vel` says which
/// way the ship is *pointed*, and `ship_vel` is what the rock's own velocity is measured
/// against. Under a floating-origin rebase they differ by the room's co-moving frame
/// velocity, which is hundreds of m/s mid-ascent.
///
/// `roll` is six independent uniforms in `[0, 1)` — the caller owns the randomness, so this
/// stays a pure function of the flight state and the field can be tested by feeding it
/// chosen numbers instead of a seeded generator. In order: scatter angle, scatter radius,
/// rock radius, closing speed, tumble direction, tumble rate.
pub fn plan_rock(
    com: Vec3,
    ship_vel: Vec3,
    true_pos: Vec3,
    true_vel: Vec3,
    d: f32,
    roll: [f32; 6],
    seed: u32,
) -> RockSpawn {
    let (forward, right, up) = flight_frame(true_pos, true_vel);
    let radial = (true_pos - crate::map::PLANET_CENTER).normalize_or(Vec3::Y);

    let closing = lerp(CLOSING_EASY.0, CLOSING_HARD.0, d)
        + roll[3] * (lerp(CLOSING_EASY.1, CLOSING_HARD.1, d) - lerp(CLOSING_EASY.0, CLOSING_HARD.0, d));
    let radius = lerp(RADIUS_EASY.0, RADIUS_HARD.0, d)
        + roll[2] * (lerp(RADIUS_EASY.1, RADIUS_HARD.1, d) - lerp(RADIUS_EASY.0, RADIUS_HARD.0, d));

    // The rock's motion **relative to the ship** — the only velocity the encounter is
    // defined in terms of. Its absolute velocity is this plus the ship's, below.
    let approach = -forward * closing - radial * FALL_SPEED;
    let track = approach.normalize_or(-forward);

    // Uniform over the scatter *disc*, not over its radius — `sqrt` is what keeps rocks
    // from piling up on the axis, which would make the field feel far denser near the nose
    // than the scatter number says. The disc is spanned in the pilot's frame (so the
    // scatter is even across the *view*) but then flattened perpendicular to the rock's own
    // track, which is what makes `scatter` mean exactly what the constants claim it does:
    // the rock's closest approach to the ship. Measured against the flight direction
    // instead, a falling rock's offset would be dominated by the 200 m of radial step-back
    // below and every rock would sail overhead.
    let angle = roll[0] * std::f32::consts::TAU;
    let scatter = lerp(SCATTER_EASY, SCATTER_HARD, d) * roll[1].sqrt();
    let spoke = right * angle.cos() + up * angle.sin();
    let across = (spoke - track * track.dot(spoke)).normalize_or_zero() * scatter;

    // Place the rock on a true intercept and step it back along its *relative* track: it is
    // exactly `WARNING_SECS` from meeting the ship, whatever combination of closing and
    // falling it is doing and however fast the ship itself is going. Only then is it pushed
    // `across` that track, which turns the intercept into the miss (or the near miss) the
    // pilot is actually flying against — and because the push is perpendicular to the
    // track, the fuse survives it untouched.
    let position = com - approach * WARNING_SECS + across;
    let velocity = ship_vel + approach;

    let tumble_angle = roll[4] * std::f32::consts::TAU;
    let spin = (right * tumble_angle.cos() + up * tumble_angle.sin()) * (roll[5] * MAX_TUMBLE);

    RockSpawn { position, velocity, spin, radius, seed }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::{GRAVITY_REF_RADIUS, PLANET_CENTER};

    /// A ship mid-climb: room-local COM at the origin doing 760 m/s, in a room whose
    /// floating-origin frame is co-moving — so its local velocity and its true velocity are
    /// deliberately *different*, which is the case the placement has to get right.
    fn climbing() -> (Vec3, Vec3, Vec3, Vec3) {
        let true_pos = PLANET_CENTER + Vec3::Y * (GRAVITY_REF_RADIUS + 4_000.0);
        let true_vel = Vec3::new(300.0, 700.0, 0.0);
        let ship_vel = Vec3::new(40.0, 90.0, 0.0);
        (Vec3::ZERO, ship_vel, true_pos, true_vel)
    }

    /// The clock holds its fire, then keeps its own schedule: one rock per call, never a
    /// burst to catch up, and never faster than the curve's interval allows.
    #[test]
    fn the_clock_holds_off_then_keeps_the_curves_pace() {
        let mut clock = FieldClock::default();
        let dt = 1.0 / 60.0;
        let mut fired: Vec<f32> = Vec::new();
        for _ in 0..(180.0 / dt) as usize {
            if clock.tick(dt).is_some() {
                fired.push(clock.elapsed);
            }
        }
        assert!(fired[0] >= FIRST_ROCK_SECS, "first rock came early at {}", fired[0]);
        assert!(fired[0] < FIRST_ROCK_SECS + 0.1, "first rock came late at {}", fired[0]);
        for pair in fired.windows(2) {
            let gap = pair[1] - pair[0];
            let mean = spawn_interval(difficulty(pair[0]));
            assert!(gap >= mean * 0.6 - 0.05 && gap <= mean * 1.4 + 0.05, "gap {gap} vs {mean}");
        }
        // A stalled owner that hands over a whole second at once still gets one rock, not
        // the handful it "owes".
        let mut stalled = FieldClock::default();
        assert!(stalled.tick(30.0).is_some());
        assert!(stalled.tick(1.0).is_none() || stalled.until_next() > 0.0);
    }

    #[test]
    fn the_field_holds_off_then_ramps_and_saturates() {
        assert_eq!(difficulty(0.0), 0.0);
        assert_eq!(difficulty(FIRST_ROCK_SECS), 0.0);
        assert!((difficulty(FIRST_ROCK_SECS + RAMP_SECS / 2.0) - 0.5).abs() < 1e-5);
        assert_eq!(difficulty(FIRST_ROCK_SECS + RAMP_SECS), 1.0);
        assert_eq!(difficulty(10_000.0), 1.0);
        // The ramp only ever makes things harder.
        let mut last = f32::MAX;
        for step in 0..200 {
            let interval = spawn_interval(difficulty(step as f32));
            assert!(interval <= last + 1e-6, "interval grew at t={step}");
            last = interval;
        }
    }

    /// Warning time is the invariant the whole placement is built around: however fast a
    /// rock closes, the pilot gets [`WARNING_SECS`] to move.
    #[test]
    fn every_rock_arrives_on_the_same_fuse() {
        let (com, ship_vel, true_pos, true_vel) = climbing();
        for d in [0.0, 0.25, 0.5, 0.75, 1.0] {
            for spread in 0..5 {
                let r = spread as f32 / 5.0;
                // Head-on (no scatter) so the whole separation is closing distance.
                let rock =
                    plan_rock(com, ship_vel, true_pos, true_vel, d, [0.0, 0.0, r, r, 0.0, 0.0], 7);
                // Measured in the SHIP's frame, which is the frame the fuse is defined in.
                let approach = rock.velocity - ship_vel;
                let fuse = (com - rock.position).length() / approach.length();
                assert!(
                    (fuse - WARNING_SECS).abs() < 1e-2,
                    "d={d} r={r}: fuse {fuse} s"
                );
            }
        }
    }

    /// The encounter must not depend on how fast the ship happens to be going. This is the
    /// regression for the first cut, which placed rocks in the flight frame but launched
    /// them with world velocities: aimed at where the ship *was*, every rock missed by
    /// `speed × WARNING_SECS` — 1.4 km at orbital speed — and a measured 131-rock flight
    /// went by without a single threat.
    #[test]
    fn a_head_on_rock_intercepts_at_any_ship_speed() {
        let true_pos = PLANET_CENTER + Vec3::Y * (GRAVITY_REF_RADIUS + 4_000.0);
        for speed in [0.0f32, 50.0, 400.0, 3_000.0] {
            let true_vel = Vec3::new(0.3, 0.7, 0.0).normalize() * speed.max(1.0);
            // Local velocity deliberately unequal to the true one (a co-moving room).
            let ship_vel = true_vel * 0.2;
            let rock = plan_rock(
                Vec3::ZERO,
                ship_vel,
                true_pos,
                true_vel,
                0.5,
                [0.0, 0.0, 0.5, 0.5, 0.0, 0.0],
                3,
            );
            // Fly both bodies forward ballistically for the fuse and see how close they get.
            let closest = (0..=350)
                .map(|step| {
                    let t = step as f32 * WARNING_SECS / 350.0;
                    (rock.position + rock.velocity * t).distance(ship_vel * t)
                })
                .fold(f32::MAX, f32::min);
            assert!(
                closest < 1.0,
                "ship at {speed} m/s: head-on rock missed by {closest} m"
            );
        }
    }

    /// The sweep must clear a rock that has gone by, and must not clear one still inbound —
    /// that is what keeps the live population set by the difficulty curve rather than by
    /// how fast the rocks happen to be travelling.
    #[test]
    fn only_a_rock_that_has_passed_is_swept() {
        let ship = Vec3::ZERO;
        let ship_vel = Vec3::new(0.0, 400.0, 0.0);
        let inbound = ship + Vec3::Y * 800.0;
        let approach = Vec3::new(0.0, -150.0, 0.0);
        assert!(!rock_is_spent(inbound, ship_vel + approach, ship, ship_vel));
        // Same rock, now well behind and still receding.
        let behind = ship - Vec3::Y * (PAST_RADIUS + 50.0);
        assert!(rock_is_spent(behind, ship_vel + approach, ship, ship_vel));
        // Just past the ship but not yet clear: kept, so a near miss stays in frame.
        let alongside = ship - Vec3::Y * (PAST_RADIUS - 50.0);
        assert!(!rock_is_spent(alongside, ship_vel + approach, ship, ship_vel));
        // Far away in any direction, whatever it is doing.
        let far = ship + Vec3::X * (DESPAWN_RADIUS + 1.0);
        assert!(rock_is_spent(far, ship_vel - approach, ship, ship_vel));
    }

    /// A harder field is tighter, bigger, and faster — the three knobs that make it a
    /// gauntlet rather than scenery.
    #[test]
    fn the_field_tightens_as_it_ramps() {
        let (com, ship_vel, true_pos, true_vel) = climbing();
        // Same rolls, different intensity: only `d` moves.
        let roll = [0.3, 1.0, 1.0, 1.0, 0.0, 0.0];
        let easy = plan_rock(com, ship_vel, true_pos, true_vel, 0.0, roll, 1);
        let hard = plan_rock(com, ship_vel, true_pos, true_vel, 1.0, roll, 1);
        // Closest approach: the perpendicular distance from the ship to the rock's track,
        // in the ship's frame.
        let miss = |r: &RockSpawn| {
            let track = (r.velocity - ship_vel).normalize();
            let offset = r.position - com;
            (offset - track * offset.dot(track)).length()
        };
        assert!(miss(&hard) < miss(&easy), "hard field scattered wider than the easy one");
        assert!(hard.radius > easy.radius, "hard rocks are not bigger");
        assert!(
            (hard.velocity - ship_vel).length() > (easy.velocity - ship_vel).length(),
            "hard rocks do not close faster"
        );
        assert!(miss(&easy) <= SCATTER_EASY + 1e-3 && miss(&hard) <= SCATTER_HARD + 1e-3);
    }

    /// Rocks fall: every one carries a component down the planet's radius, so the field
    /// crosses the ship's path instead of merely running at it.
    #[test]
    fn rocks_fall_toward_the_planet() {
        let (com, ship_vel, true_pos, true_vel) = climbing();
        let radial = (true_pos - PLANET_CENTER).normalize();
        for i in 0..8 {
            let t = i as f32 / 8.0;
            let rock = plan_rock(com, ship_vel, true_pos, true_vel, t, [t, t, t, t, t, t], i);
            // Relative to the ship — which is climbing, so an absolute test would only be
            // measuring the ship.
            assert!((rock.velocity - ship_vel).dot(radial) < 0.0, "rock {i} was rising");
        }
    }
}
