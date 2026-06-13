#version 300 es

precision highp float;
precision highp sampler3D;

uniform sampler2D tex;
uniform sampler3D color_lut;
uniform float alpha;

in vec2 v_coords;
out vec4 frag_color;

void main() {
    vec4 color = texture(tex, v_coords);

    // Unpremultiply alpha before LUT lookup — the LUT expects
    // straight color values, not premultiplied.
    vec3 straight = color.a > 0.0 ? color.rgb / color.a : vec3(0.0);

    // Apply 3D LUT color correction.
    vec3 corrected = texture(color_lut, straight).rgb;

    // Re-premultiply alpha.
    frag_color = vec4(corrected * color.a, color.a) * alpha;
}
