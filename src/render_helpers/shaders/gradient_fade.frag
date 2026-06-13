#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// x is left edge, y is right edge of the gradient.
uniform vec2 cutoff;

// SDR reference white luminance in cd/m². 0.0 = SDR mode (no linearization).
uniform float ref_lum;

// sRGB EOTF: sRGB signal -> linear
vec3 srgb_eotf(vec3 v) {
    return mix(
        v / 12.92,
        pow((v + 0.055) / 1.055, vec3(2.4)),
        step(0.04045, v)
    );
}

void main() {
    // Sample the texture.
    vec4 color = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0);
#endif

    // HDR linearization: decode sRGB to linear light and scale to absolute cd/m².
    if (ref_lum > 0.0) {
        vec3 straight = color.a > 0.0 ? color.rgb / color.a : vec3(0.0);
        color = vec4(srgb_eotf(straight) * ref_lum * color.a, color.a);
    }

    if (cutoff.x < cutoff.y) {
        float fade = clamp((cutoff.y - v_coords.x) / (cutoff.y - cutoff.x), 0.0, 1.0);
        color = color * fade;
    }

    // Apply final alpha and tint.
    color = color * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
