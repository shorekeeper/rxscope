#version 450

// Positions arrive in framebuffer pixels with the origin at the top left.
// Vulkan clip space has y pointing down as well, so the mapping is direct
// and no projection matrix is needed.

layout(location = 0) in vec2 in_pos;
layout(location = 1) in vec2 in_uv;
layout(location = 2) in vec4 in_color;   // R8G8B8A8_UNORM, straight alpha
layout(location = 3) in uint in_mode;

layout(location = 0) out vec2 frag_uv;
layout(location = 1) out vec4 frag_color;
layout(location = 2) flat out uint frag_mode;

// The block is declared identically in both stages because the push constant
// range covers both. Only the viewport is read here; the rest belongs to the
// palette mapping and is consumed by the fragment stage.
layout(push_constant) uniform Push {
    vec2 viewport;
    float palette_scale;
    float palette_span;
    float palette_gamma;
    float reserved;
} pc;

void main() {
    vec2 ndc = in_pos / pc.viewport * 2.0 - 1.0;
    gl_Position = vec4(ndc, 0.0, 1.0);
    frag_uv = in_uv;
    frag_color = in_color;
    frag_mode = in_mode;
}
