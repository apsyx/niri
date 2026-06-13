#version 300 es

precision highp float;

uniform sampler2D tex;

// Transfer function identifiers:
// 0 = sRGB, 1 = PQ (ST 2084), 2 = HLG, 3 = Linear
uniform int src_tf;
uniform int dst_tf;
uniform float src_max_lum;  // Source max luminance in cd/m²
uniform float dst_max_lum;  // Destination max luminance in cd/m²
uniform mat3 color_matrix;  // Gamut conversion matrix (identity if same gamut)

in vec2 v_coords;
out vec4 frag_color;

// PQ constants (SMPTE ST 2084)
const float PQ_M1 = 0.1593017578125;
const float PQ_M2 = 78.84375;
const float PQ_C1 = 0.8359375;
const float PQ_C2 = 18.8515625;
const float PQ_C3 = 18.6875;

// sRGB EOTF: sRGB signal -> linear
vec3 srgb_eotf(vec3 v) {
    return mix(
        v / 12.92,
        pow((v + 0.055) / 1.055, vec3(2.4)),
        step(0.04045, v)
    );
}

// sRGB OETF: linear -> sRGB signal
vec3 srgb_oetf(vec3 v) {
    v = clamp(v, 0.0, 1.0);
    return mix(
        v * 12.92,
        1.055 * pow(v, vec3(1.0 / 2.4)) - 0.055,
        step(0.0031308, v)
    );
}

// PQ EOTF: PQ signal -> absolute luminance (0..10000 cd/m²)
vec3 pq_eotf(vec3 v) {
    vec3 vp = pow(max(v, vec3(0.0)), vec3(1.0 / PQ_M2));
    vec3 n = max(vp - PQ_C1, vec3(0.0)) / (PQ_C2 - PQ_C3 * vp);
    return 10000.0 * pow(n, vec3(1.0 / PQ_M1));
}

// PQ OETF: absolute luminance (0..10000 cd/m²) -> PQ signal
vec3 pq_oetf(vec3 v) {
    vec3 y = pow(max(v, vec3(0.0)) / 10000.0, vec3(PQ_M1));
    return pow((PQ_C1 + PQ_C2 * y) / (1.0 + PQ_C3 * y), vec3(PQ_M2));
}

// HLG OETF: scene-referred linear -> HLG signal
vec3 hlg_oetf(vec3 v) {
    const float a = 0.17883277;
    const float b = 0.28466892;
    const float c = 0.55991073;
    return mix(
        sqrt(3.0 * v),
        a * log(12.0 * v - b) + c,
        step(1.0 / 12.0, v)
    );
}

// HLG EOTF inverse (simplified): HLG signal -> scene-referred linear
vec3 hlg_eotf(vec3 v) {
    const float a = 0.17883277;
    const float b = 0.28466892;
    const float c = 0.55991073;
    return mix(
        v * v / 3.0,
        (exp((v - c) / a) + b) / 12.0,
        step(0.5, v)
    );
}

// Simple Reinhard tone mapping: maps [0, inf) -> [0, 1)
vec3 reinhard(vec3 v, float max_lum) {
    return v / (v + vec3(max_lum));
}

// Decode from source transfer function to linear luminance (cd/m²).
vec3 decode_tf(vec3 signal, int tf, float max_lum) {
    if (tf == 0) {
        // sRGB: normalized to max_lum (typically 80 cd/m²)
        return srgb_eotf(signal) * max_lum;
    } else if (tf == 1) {
        // PQ: already absolute luminance
        return pq_eotf(signal);
    } else if (tf == 2) {
        // HLG
        return hlg_eotf(signal) * max_lum;
    } else {
        // Linear
        return signal * max_lum;
    }
}

// Encode from linear luminance (cd/m²) to destination transfer function.
vec3 encode_tf(vec3 linear, int tf, float max_lum) {
    if (tf == 0) {
        // sRGB
        return srgb_oetf(clamp(linear / max_lum, 0.0, 1.0));
    } else if (tf == 1) {
        // PQ
        return pq_oetf(linear);
    } else if (tf == 2) {
        // HLG
        return hlg_oetf(clamp(linear / max_lum, 0.0, 1.0));
    } else {
        // Linear
        return clamp(linear / max_lum, 0.0, 1.0);
    }
}

void main() {
    vec4 color = texture(tex, v_coords);

    // Unpremultiply alpha — the EOTFs are nonlinear and must operate
    // on straight (not premultiplied) color values.
    vec3 straight = color.a > 0.0 ? color.rgb / color.a : vec3(0.0);

    // Decode to absolute linear luminance.
    vec3 linear = decode_tf(straight, src_tf, src_max_lum);

    // Apply gamut conversion in linear light.
    linear = color_matrix * linear;

    // Tone map if destination max luminance is lower than source.
    if (dst_max_lum < src_max_lum) {
        // Reinhard tone mapping to compress dynamic range.
        float scale = dst_max_lum / src_max_lum;
        linear = linear * scale / (linear / src_max_lum + vec3(scale));
    }

    // Encode to destination transfer function.
    vec3 encoded = encode_tf(linear, dst_tf, dst_max_lum);

    // Re-premultiply alpha.
    frag_color = vec4(encoded * color.a, color.a);
}
