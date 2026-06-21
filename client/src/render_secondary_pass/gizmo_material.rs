use bevy::{
    prelude::*,
    reflect::TypeUuid,
    render::render_resource::{AsBindGroup, ShaderRef},
};

pub const GIZMO_SHADER_HANDLE: HandleUntyped =
    HandleUntyped::weak_from_u64(Shader::TYPE_UUID, 13953800272683943019);

#[derive(Debug, Clone, Default, AsBindGroup, TypeUuid)]
#[uuid = "0cf245a7-ce7a-4473-821c-111e6f359193"]
pub struct GizmoMaterial {
    #[uniform(0)]
    pub color: Color,
}

impl From<Color> for GizmoMaterial {
    fn from(color: Color) -> Self {
        GizmoMaterial { color }
    }
}

impl Material for GizmoMaterial {
    fn vertex_shader() -> ShaderRef {
        GIZMO_SHADER_HANDLE.typed().into()
    }

    fn fragment_shader() -> ShaderRef {
        GIZMO_SHADER_HANDLE.typed().into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }
}
