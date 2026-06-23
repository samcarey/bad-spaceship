use bevy::{
    asset::uuid_handle,
    prelude::*,
    reflect::TypePath,
    render::render_resource::AsBindGroup,
    // Bevy 0.17 moved shader types (incl. `ShaderRef`) to `bevy_shader` (`bevy::shader`).
    shader::ShaderRef,
};

// Bevy 0.16 deprecated `Handle::weak_from_u128` in favour of the `weak_handle!`
// macro (a UUID string); Bevy 0.17 renamed that macro to `uuid_handle!` (the
// `Handle::Weak` variant became `Handle::Uuid`). Same id the u128 seed produced,
// written as a UUID, so the registered shader handle is byte-for-byte unchanged.
pub const GIZMO_SHADER_HANDLE: Handle<Shader> =
    uuid_handle!("00000000-0000-0000-c1a5-db6ae813446b");

// Bevy 0.12's asset rework replaced `TypeUuid` with the `Asset` derive (which,
// like `Material`, still requires `TypePath`).
#[derive(Asset, Debug, Clone, Default, AsBindGroup, TypePath)]
pub struct GizmoMaterial {
    // Bevy 0.14's color overhaul dropped `ShaderType` for the `Color` enum, so it
    // can no longer back a `#[uniform]` directly. Store `LinearRgba` — the exact
    // representation 0.13's `Color` uniform already serialized to — so the shader's
    // `vec4<f32>` and the on-screen colour are unchanged.
    #[uniform(0)]
    pub color: LinearRgba,
}

impl From<Color> for GizmoMaterial {
    fn from(color: Color) -> Self {
        GizmoMaterial {
            color: color.to_linear(),
        }
    }
}

impl Material for GizmoMaterial {
    fn vertex_shader() -> ShaderRef {
        GIZMO_SHADER_HANDLE.into()
    }

    fn fragment_shader() -> ShaderRef {
        GIZMO_SHADER_HANDLE.into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }
}
