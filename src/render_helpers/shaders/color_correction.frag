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

    // Apply 3D LUT color correction.
    // The LUT is indexed by the linear RGB values.
    vec3 corrected = texture(color_lut, color.rgb).rgb;

    frag_color = vec4(corrected * color.a, color.a) * alpha;
}
