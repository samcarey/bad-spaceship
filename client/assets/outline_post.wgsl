// Screen-space silhouette outline compositing pass (see `client/src/outline.rs`).
//
// Runs as a full-screen pass over the main camera's texture. It reads only a
// coverage `mask_texture` (alpha = 1 where an outlined part draws, produced by a
// second camera that renders just the outlined parts) and outputs the outline
// colour with alpha, ALPHA-BLENDED over the scene — so it never reads the scene
// texture (which lets it avoid `post_process_write`, unsupported on WebGL2).
//
// For each pixel NOT itself covered, it checks a ring of samples within `radius`
// pixels; if any is covered, this pixel is on the outside edge of an outlined
// part and is painted the outline colour. The result is a line of CONSTANT
// screen-pixel width (the dilation radius) hugging the on-screen silhouette,
// composited on top so the ground the part rests on can never hide it.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var mask_texture: texture_2d<f32>;
@group(0) @binding(1) var texture_sampler: sampler;
struct OutlineSettings {
    color: vec4<f32>,   // rgb = outline colour, a = opacity
    params: vec4<f32>,  // x = ring radius in pixels
};
@group(0) @binding(2) var<uniform> settings: OutlineSettings;

fn coverage(uv: vec2<f32>) -> f32 {
    // textureSampleLevel (explicit LOD, no derivatives) — valid under the
    // non-uniform control flow below and on WebGL2.
    return textureSampleLevel(mask_texture, texture_sampler, uv, 0.0).a;
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    // Inside an outlined part: fully transparent (the outline is a rim OUTSIDE the
    // silhouette, so the part itself is never covered).
    if coverage(in.uv) > 0.5 {
        return vec4<f32>(0.0);
    }

    let radius = settings.params.x;
    let texel = radius / vec2<f32>(textureDimensions(mask_texture));

    // Dilate: if any covered texel sits within `radius` pixels, this pixel is on
    // the outline ring. Sample a ring plus a half-radius ring so thin gaps fill.
    var hit = 0.0;
    for (var i = 0; i < 12; i = i + 1) {
        let a = f32(i) / 12.0 * 6.28318530718;
        let dir = vec2<f32>(cos(a), sin(a));
        hit = max(hit, coverage(in.uv + dir * texel));
        hit = max(hit, coverage(in.uv + dir * texel * 0.5));
    }

    // Premultiplied? No — standard alpha blend (src_alpha, 1-src_alpha) is set on
    // the pipeline, so output straight colour with the ring alpha.
    return vec4<f32>(settings.color.rgb, settings.color.a * hit);
}
