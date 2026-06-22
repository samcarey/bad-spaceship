use bevy::{
    prelude::*,
    reflect::TypePath,
    render::render_resource::{AsBindGroup, ShaderRef},
};

// Bevy 0.12 dropped the UUID-based `HandleUntyped`; internal assets now use a
// typed weak handle seeded from a u128.
pub const GIZMO_SHADER_HANDLE: Handle<Shader> = Handle::weak_from_u128(13953800272683943019);

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
