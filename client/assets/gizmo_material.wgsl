#import bevy_pbr::mesh_view_bindings
#import bevy_pbr::mesh_bindings

struct GizmoMaterial {
    color: vec4<f32>,
};

@group(1) @binding(0)
var<uniform> material: GizmoMaterial;

struct Vertex {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    let world_position = mesh.model * vec4<f32>(vertex.position, 1.0);
    var out: VertexOutput;
    var modified_clip = view.view_proj * world_position;
    // Remap the depth to be right in front of the camera. We remap (mix) here instead of hardcoding
    // the depth, to ensure the components of the gizmo mesh are sorted correctly.
    modified_clip.z = mix(0.999, 1.0, modified_clip.z);
    out.clip_position = modified_clip;
    out.uv = vertex.uv;
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    return material.color;
}
