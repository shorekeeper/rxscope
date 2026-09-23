#version 450

layout(location = 0) in vec2 frag_uv;
layout(location = 1) in vec4 frag_color;
layout(location = 2) flat in uint frag_mode;

layout(location = 0) out vec4 out_color;

layout(set = 0, binding = 0) uniform sampler2D tex;

// Palette for the waterfall, bound once per frame rather than per texture: it
// is the same for every draw command, and a per texture binding would have to
// be written into the set of every glyph atlas as well.
layout(set = 1, binding = 0) uniform sampler2D palette;

layout(push_constant) uniform Push {
    vec2 viewport;
    float palette_scale;
    float palette_span;
    float palette_gamma;
    float reserved;
} pc;

// Mode selects how the bound texture contributes:
//   0 solid, texture ignored, the default white 1x1 is bound;
//   1 alpha mask, red channel is coverage, used by the glyph atlas;
//   2 full RGBA texture modulated by the vertex color;
//   3 single channel expanded to luminance;
//   4 single channel mapped through the palette.
const uint MODE_SOLID   = 0u;
const uint MODE_ALPHA   = 1u;
const uint MODE_RGBA    = 2u;
const uint MODE_LUMA    = 3u;
const uint MODE_PALETTE = 4u;

void main() {
    if (frag_mode == MODE_SOLID) {
        out_color = frag_color;
    } else if (frag_mode == MODE_ALPHA) {
        float a = texture(tex, frag_uv).r;
        out_color = vec4(frag_color.rgb, frag_color.a * a);
    } else if (frag_mode == MODE_RGBA) {
        out_color = frag_color * texture(tex, frag_uv);
    } else if (frag_mode == MODE_LUMA) {
        float l = texture(tex, frag_uv).r;
        out_color = vec4(frag_color.rgb * l, frag_color.a);
    } else {
        // The stored byte scaled, spanned and shaped. Applying all three here
        // rather than at write time is what lets any of them move without
        // rewriting the texture.
        float stored = texture(tex, frag_uv).r;
        float t = clamp(stored * pc.palette_scale / max(pc.palette_span, 1e-6), 0.0, 1.0);
        t = pow(t, pc.palette_gamma);
        out_color = frag_color * texture(palette, vec2(t, 0.5));
    }

    // Fully transparent fragments are dropped so overlapping panels do not
    // accumulate blend cost on tiled GPUs.
    if (out_color.a <= 0.0) {
        discard;
    }
}
