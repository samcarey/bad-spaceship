//! Assembly detection: group parts into connected "assemblies" through their joints
//! and find, per room, the largest such group (≥ 2 parts) and its mass-weighted
//! centre of mass.
//!
//! Pure index math — no ECS or lightyear types — so the multiplayer **server**
//! (`mark_largest_assembly`, per room) and the single-player **client**
//! (the thrust-vector + centre-of-mass orb, one room) share **one** implementation,
//! and one unit-tested definition of "what counts as an assembly". Callers control
//! which joints become edges, so excluding ground joints (the client filters them;
//! the server never creates them) is a property of the edge list, not this core.

use bevy::prelude::Vec3;
use std::collections::HashMap;
use std::hash::Hash;

/// A tiny union-find (disjoint-set) over item indices, used to group parts into
/// connected assemblies by their joints. Path-compression on `find` keeps it flat;
/// no union-by-rank needed at these sizes (≤ a few hundred parts).
pub struct DisjointSet {
    parent: Vec<usize>,
}

impl DisjointSet {
    pub fn new(n: usize) -> Self {
        Self { parent: (0..n).collect() }
    }
    pub fn find(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }
    pub fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[ra] = rb;
        }
    }
}

/// The winning assembly for a room: its mass-weighted center of mass and the indices
/// (into the `items` slice) of its member parts.
pub struct Assembly {
    pub com: Vec3,
    pub members: Vec<usize>,
}

/// Given each part as `(world position, mass weight, room)` and the joint edges (index
/// pairs into `items`), return — for every room that has an assembly of ≥ 2 jointed
/// parts — the largest such component's mass-weighted center of mass and member
/// indices. Generic over the room key so single-player callers can use `()`.
pub fn largest_assembly_per_room<R: Copy + Eq + Hash>(
    items: &[(Vec3, f32, R)],
    edges: &[(usize, usize)],
) -> HashMap<R, Assembly> {
    // Union the parts each joint connects into disjoint sets.
    let mut dsu = DisjointSet::new(items.len());
    for &(a, b) in edges {
        dsu.union(a, b);
    }
    // Collect each connected component's members (all share a room).
    let mut components: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..items.len() {
        components.entry(dsu.find(i)).or_default().push(i);
    }
    // Per room, keep the largest component of ≥ 2 parts (a lone part isn't an assembly).
    let mut best_by_room: HashMap<R, &Vec<usize>> = HashMap::new();
    for members in components.values() {
        if members.len() < 2 {
            continue;
        }
        let room = items[members[0]].2;
        let entry = best_by_room.entry(room).or_insert(members);
        if members.len() > entry.len() {
            *entry = members;
        }
    }
    // Mass-weight each winner's center of mass (uniform density ⇒ volume ∝ mass).
    let mut out: HashMap<R, Assembly> = HashMap::new();
    for (room, members) in best_by_room {
        let mut weight_sum = 0.0;
        let mut weighted_pos = Vec3::ZERO;
        for &i in members {
            weighted_pos += items[i].0 * items[i].1;
            weight_sum += items[i].1;
        }
        let com = if weight_sum > 0.0 { weighted_pos / weight_sum } else { Vec3::ZERO };
        out.insert(room, Assembly { com, members: members.clone() });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{largest_assembly_per_room, Vec3};

    /// Two assemblies in one room + a lone part + a second room: the largest
    /// component per room wins, and its center of mass is mass-weighted.
    #[test]
    fn picks_largest_assembly_with_weighted_com() {
        // (position, mass weight, room)
        let items = vec![
            (Vec3::new(0.0, 0.0, 0.0), 1.0, 0u32), // 0 room0 chain A (2 parts)
            (Vec3::new(2.0, 0.0, 0.0), 1.0, 0u32), // 1 room0 chain A
            (Vec3::new(0.0, 0.0, 0.0), 1.0, 0u32), // 2 room0 chain B (3 parts, winner)
            (Vec3::new(4.0, 0.0, 0.0), 3.0, 0u32), // 3 room0 chain B (heavier)
            (Vec3::new(2.0, 0.0, 0.0), 1.0, 0u32), // 4 room0 chain B
            (Vec3::new(9.0, 0.0, 0.0), 1.0, 0u32), // 5 room0 lone part (no joint)
            (Vec3::new(10.0, 0.0, 0.0), 1.0, 1u32), // 6 room1 (2 parts)
            (Vec3::new(12.0, 0.0, 0.0), 1.0, 1u32), // 7 room1
        ];
        // Chain A: 0–1. Chain B: 2–3–4. Room 1: 6–7.
        let edges = vec![(0, 1), (2, 3), (3, 4), (6, 7)];
        let out = largest_assembly_per_room(&items, &edges);

        // Room 0's winner is the 3-part chain B {2,3,4}, not the 2-part chain A.
        let r0 = out.get(&0).expect("room 0 has an assembly");
        let mut members = r0.members.clone();
        members.sort();
        assert_eq!(members, vec![2, 3, 4]);
        // Mass-weighted COM on x: (0·1 + 4·3 + 2·1) / (1+3+1) = 14/5 = 2.8.
        assert!((r0.com.x - 2.8).abs() < 1e-5, "com.x = {}", r0.com.x);
        assert!(r0.com.y.abs() < 1e-6 && r0.com.z.abs() < 1e-6);

        // Room 1's only assembly {6,7} → COM midway at x = 11.
        let r1 = out.get(&1).expect("room 1 has an assembly");
        assert_eq!(r1.members.len(), 2);
        assert!((r1.com.x - 11.0).abs() < 1e-5, "com.x = {}", r1.com.x);
    }

    /// No joints ⇒ every part is a singleton ⇒ no assembly anywhere.
    #[test]
    fn no_joints_means_no_assembly() {
        let items = vec![
            (Vec3::ZERO, 1.0, 0u32),
            (Vec3::X, 1.0, 0u32),
            (Vec3::Y, 1.0, 1u32),
        ];
        assert!(largest_assembly_per_room(&items, &[]).is_empty());
    }
}
