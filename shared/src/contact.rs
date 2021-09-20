use bevy::prelude::*;
use bevy_rapier3d::{physics::IntoHandle, prelude::NarrowPhase};

use crate::TouchingColliders;

pub struct ContactPlugin;

impl Plugin for ContactPlugin {
    fn build(&self, app: &mut AppBuilder) {
        app.add_system(track_contact.system());
    }
}

fn track_contact(
    mut primary_entities: Query<(Entity, &mut TouchingColliders)>,
    narrow_phase: Res<NarrowPhase>,
) {
    for (primary_entity, mut touching) in primary_entities.iter_mut() {
        touching.0 = Vec::new();
        for contact_pair in narrow_phase.contacts_with(primary_entity.handle()) {
            if contact_pair.has_any_active_contact {
                let other_collider = if contact_pair.collider1 == primary_entity.handle() {
                    contact_pair.collider2
                } else {
                    contact_pair.collider1
                };
                touching.0.push(other_collider);
            }
        }
    }
}
