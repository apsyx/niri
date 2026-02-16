use std::path::Path;

use anyhow::Context as _;

/// Well-known primaries constants.
pub const SRGB_PRIMARIES: Primaries = Primaries {
    r_x: 0.64,
    r_y: 0.33,
    g_x: 0.30,
    g_y: 0.60,
    b_x: 0.15,
    b_y: 0.06,
    w_x: 0.3127,
    w_y: 0.3290,
};

pub const BT2020_PRIMARIES: Primaries = Primaries {
    r_x: 0.708,
    r_y: 0.292,
    g_x: 0.170,
    g_y: 0.797,
    b_x: 0.131,
    b_y: 0.046,
    w_x: 0.3127,
    w_y: 0.3290,
};

pub const DISPLAY_P3_PRIMARIES: Primaries = Primaries {
    r_x: 0.680,
    r_y: 0.320,
    g_x: 0.265,
    g_y: 0.690,
    b_x: 0.150,
    b_y: 0.060,
    w_x: 0.3127,
    w_y: 0.3290,
};

/// CIE xy chromaticity primaries (used for color matrix computation).
#[derive(Debug, Clone, Copy)]
pub struct Primaries {
    pub r_x: f64,
    pub r_y: f64,
    pub g_x: f64,
    pub g_y: f64,
    pub b_x: f64,
    pub b_y: f64,
    pub w_x: f64,
    pub w_y: f64,
}

/// Size of each dimension in the 3D LUT (33^3 = 35,937 texels).
pub const LUT_SIZE: usize = 33;

/// Color information extracted from the EDID of a display.
#[derive(Debug, Clone)]
pub struct EdidColorInfo {
    /// CIE xy chromaticity for the red primary.
    pub red: (f64, f64),
    /// CIE xy chromaticity for the green primary.
    pub green: (f64, f64),
    /// CIE xy chromaticity for the blue primary.
    pub blue: (f64, f64),
    /// CIE xy chromaticity for the white point.
    pub white: (f64, f64),
}

/// A parsed ICC profile with a generated 3D LUT for color correction.
#[derive(Debug)]
pub struct OutputColorProfile {
    /// The 3D LUT data: `LUT_SIZE^3` RGB triplets as `[f32; 3]`, stored in R,G,B order.
    pub lut_data: Vec<[f32; 3]>,
    /// Fallback 1D gamma ramp (for when 3D LUT textures are unavailable).
    /// Contains concatenated R, G, B ramps, each of length `gamma_ramp_size`.
    pub gamma_ramp: Option<Vec<u16>>,
    pub gamma_ramp_size: usize,
}

impl OutputColorProfile {
    /// Parse an ICC profile and build a sRGB → profile transform, then sample into a 3D LUT.
    pub fn from_icc(path: &Path) -> anyhow::Result<Self> {
        let data = std::fs::read(path)
            .with_context(|| format!("failed to read ICC profile: {}", path.display()))?;

        let profile = lcms2::Profile::new_icc(&data)
            .with_context(|| format!("failed to parse ICC profile: {}", path.display()))?;

        let srgb = lcms2::Profile::new_srgb();

        // Build a transform from sRGB to the output profile.
        let transform = lcms2::Transform::new(
            &srgb,
            lcms2::PixelFormat::RGB_FLT,
            &profile,
            lcms2::PixelFormat::RGB_FLT,
            lcms2::Intent::Perceptual,
        )
        .context("failed to create lcms2 transform")?;

        // Sample the 3D LUT.
        let total = LUT_SIZE * LUT_SIZE * LUT_SIZE;
        let mut input = Vec::with_capacity(total);
        for b_i in 0..LUT_SIZE {
            for g_i in 0..LUT_SIZE {
                for r_i in 0..LUT_SIZE {
                    input.push([
                        r_i as f32 / (LUT_SIZE - 1) as f32,
                        g_i as f32 / (LUT_SIZE - 1) as f32,
                        b_i as f32 / (LUT_SIZE - 1) as f32,
                    ]);
                }
            }
        }

        let mut output = vec![[0f32; 3]; total];
        transform.transform_pixels(&input, &mut output);

        // Also build a 1D gamma ramp fallback (256 entries per channel).
        let ramp_size = 256;
        let mut gamma_ramp = Vec::with_capacity(ramp_size * 3);

        // Red ramp.
        for i in 0..ramp_size {
            let v = i as f32 / (ramp_size - 1) as f32;
            let pixel = [[v, 0.0f32, 0.0f32]];
            let mut out = [[0f32; 3]];
            transform.transform_pixels(&pixel, &mut out);
            gamma_ramp.push((out[0][0].clamp(0.0, 1.0) * 65535.0) as u16);
        }
        // Green ramp.
        for i in 0..ramp_size {
            let v = i as f32 / (ramp_size - 1) as f32;
            let pixel = [[0.0f32, v, 0.0f32]];
            let mut out = [[0f32; 3]];
            transform.transform_pixels(&pixel, &mut out);
            gamma_ramp.push((out[0][1].clamp(0.0, 1.0) * 65535.0) as u16);
        }
        // Blue ramp.
        for i in 0..ramp_size {
            let v = i as f32 / (ramp_size - 1) as f32;
            let pixel = [[0.0f32, 0.0f32, v]];
            let mut out = [[0f32; 3]];
            transform.transform_pixels(&pixel, &mut out);
            gamma_ramp.push((out[0][2].clamp(0.0, 1.0) * 65535.0) as u16);
        }

        Ok(Self {
            lut_data: output,
            gamma_ramp: Some(gamma_ramp),
            gamma_ramp_size: ramp_size,
        })
    }
}

/// Upload a 3D LUT as a GL 3D texture.
///
/// Uses the renderer's GL context to create and upload the texture.
/// Returns the raw GL texture name.
pub fn upload_3d_lut_texture(
    renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
    lut_data: &[[f32; 3]],
) -> anyhow::Result<u32> {
    use smithay::backend::renderer::gles::ffi;

    renderer
        .with_context(|gl| unsafe {
            let mut tex = 0u32;
            gl.GenTextures(1, &mut tex);
            gl.BindTexture(ffi::TEXTURE_3D, tex);
            gl.TexParameteri(ffi::TEXTURE_3D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as _);
            gl.TexParameteri(ffi::TEXTURE_3D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as _);
            gl.TexParameteri(
                ffi::TEXTURE_3D,
                ffi::TEXTURE_WRAP_S,
                ffi::CLAMP_TO_EDGE as _,
            );
            gl.TexParameteri(
                ffi::TEXTURE_3D,
                ffi::TEXTURE_WRAP_T,
                ffi::CLAMP_TO_EDGE as _,
            );
            gl.TexParameteri(
                ffi::TEXTURE_3D,
                ffi::TEXTURE_WRAP_R,
                ffi::CLAMP_TO_EDGE as _,
            );

            gl.TexImage3D(
                ffi::TEXTURE_3D,
                0,
                ffi::RGB16F as _,
                LUT_SIZE as _,
                LUT_SIZE as _,
                LUT_SIZE as _,
                0,
                ffi::RGB,
                ffi::FLOAT,
                lut_data.as_ptr() as *const _,
            );

            gl.BindTexture(ffi::TEXTURE_3D, 0);

            if tex == 0 {
                anyhow::bail!("failed to create 3D LUT texture");
            }

            Ok(tex)
        })
        .context("failed to access GL context")?
}

/// Matches `struct hdr_metadata_infoframe` from `<drm/drm_mode.h>`.
///
/// display_primaries order: [0]=red, [1]=green, [2]=blue.
/// Chromaticity values are CIE xy × 50,000.
/// Luminance: max in cd/m², min in 0.0001 cd/m².
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct HdrMetadataInfoframe {
    pub eotf: u8,
    pub metadata_type: u8,
    pub display_primaries: [HdrPrimaryChromaticity; 3],
    pub white_point: HdrPrimaryChromaticity,
    pub max_display_mastering_luminance: u16,
    pub min_display_mastering_luminance: u16,
    pub max_cll: u16,
    pub max_fall: u16,
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct HdrPrimaryChromaticity {
    pub x: u16,
    pub y: u16,
}

/// Matches `struct hdr_output_metadata` from `<drm/drm_mode.h>`.
///
/// The kernel struct is 30 bytes: 4 (metadata_type) + 26 (infoframe).
/// We use `repr(C, packed)` to match the kernel layout exactly.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct HdrOutputMetadata {
    pub metadata_type: u32,
    pub hdmi_metadata_type1: HdrMetadataInfoframe,
}

/// Build HDR output metadata for the DRM `HDR_OUTPUT_METADATA` connector property.
///
/// Uses HDMI Static Metadata Type 1 with SMPTE ST 2084 (PQ) EOTF.
/// Primaries come from EDID when available, otherwise default to BT.2020.
pub fn build_hdr_output_metadata(
    edid: Option<&EdidColorInfo>,
    max_luminance: u32,
    min_luminance: u32,
) -> HdrOutputMetadata {
    let (r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y) = if let Some(e) = edid {
        (e.red.0, e.red.1, e.green.0, e.green.1, e.blue.0, e.blue.1, e.white.0, e.white.1)
    } else {
        (
            BT2020_PRIMARIES.r_x, BT2020_PRIMARIES.r_y,
            BT2020_PRIMARIES.g_x, BT2020_PRIMARIES.g_y,
            BT2020_PRIMARIES.b_x, BT2020_PRIMARIES.b_y,
            BT2020_PRIMARIES.w_x, BT2020_PRIMARIES.w_y,
        )
    };

    let to_chr = |x: f64, y: f64| HdrPrimaryChromaticity {
        x: (x * 50000.0) as u16,
        y: (y * 50000.0) as u16,
    };

    HdrOutputMetadata {
        // HDMI_STATIC_METADATA_TYPE1
        metadata_type: 0,
        hdmi_metadata_type1: HdrMetadataInfoframe {
            // SMPTE ST 2084 (PQ)
            eotf: 2,
            // Static Metadata Type 1
            metadata_type: 0,
            // [0]=red, [1]=green, [2]=blue
            display_primaries: [
                to_chr(r_x, r_y),
                to_chr(g_x, g_y),
                to_chr(b_x, b_y),
            ],
            white_point: to_chr(w_x, w_y),
            max_display_mastering_luminance: max_luminance as u16,
            // min_luminance is in 0.0001 cd/m² units.
            min_display_mastering_luminance: min_luminance as u16,
            max_cll: max_luminance as u16,
            max_fall: (max_luminance / 4) as u16,
        },
    }
}

/// Compute a 3×3 gamut conversion matrix from source to destination primaries.
///
/// The matrix converts linear RGB in the source gamut to linear RGB in the
/// destination gamut. Computation:
///   dst_XYZ_to_RGB × Bradford_CAT × src_RGB_to_XYZ
///
/// Bradford chromatic adaptation is applied when source and destination
/// white points differ. When they are the same (e.g. both D65), the
/// adaptation matrix is the identity.
pub fn gamut_conversion_matrix(src: &Primaries, dst: &Primaries) -> [[f32; 3]; 3] {
    let src_mat = rgb_to_xyz_matrix(src);
    let dst_mat = rgb_to_xyz_matrix(dst);
    let dst_inv = invert_3x3_f64(dst_mat);

    // Bradford chromatic adaptation transform.
    let cat = bradford_cat(
        xy_to_xyz(src.w_x, src.w_y),
        xy_to_xyz(dst.w_x, dst.w_y),
    );

    // Result = dst_inv × cat × src_mat
    let cat_src = mul_3x3_f64(cat, src_mat);
    mul_3x3_f64_to_f32(dst_inv, cat_src)
}

/// Bradford chromatic adaptation matrix from source to destination white point.
/// If white points are the same, returns identity.
fn bradford_cat(src_w: [f64; 3], dst_w: [f64; 3]) -> [[f64; 3]; 3] {
    // Bradford cone-response matrix.
    const M: [[f64; 3]; 3] = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];

    let src_cone = [
        M[0][0] * src_w[0] + M[0][1] * src_w[1] + M[0][2] * src_w[2],
        M[1][0] * src_w[0] + M[1][1] * src_w[1] + M[1][2] * src_w[2],
        M[2][0] * src_w[0] + M[2][1] * src_w[1] + M[2][2] * src_w[2],
    ];
    let dst_cone = [
        M[0][0] * dst_w[0] + M[0][1] * dst_w[1] + M[0][2] * dst_w[2],
        M[1][0] * dst_w[0] + M[1][1] * dst_w[1] + M[1][2] * dst_w[2],
        M[2][0] * dst_w[0] + M[2][1] * dst_w[1] + M[2][2] * dst_w[2],
    ];

    // Check if white points are effectively the same (skip adaptation).
    let eps = 1e-10;
    if (src_cone[0] - dst_cone[0]).abs() < eps
        && (src_cone[1] - dst_cone[1]).abs() < eps
        && (src_cone[2] - dst_cone[2]).abs() < eps
    {
        return [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ];
    }

    // Scale matrix: diag(dst_cone / src_cone)
    let scale = [
        [dst_cone[0] / src_cone[0], 0.0, 0.0],
        [0.0, dst_cone[1] / src_cone[1], 0.0],
        [0.0, 0.0, dst_cone[2] / src_cone[2]],
    ];

    // M^-1 * scale * M
    let m_inv = invert_3x3_f64(M);
    let sm = mul_3x3_f64(scale, M);
    mul_3x3_f64(m_inv, sm)
}

/// Build the RGB-to-XYZ matrix from primaries.
fn rgb_to_xyz_matrix(p: &Primaries) -> [[f64; 3]; 3] {
    // Convert xy chromaticity to XYZ (Y=1).
    let r_xyz = xy_to_xyz(p.r_x, p.r_y);
    let g_xyz = xy_to_xyz(p.g_x, p.g_y);
    let b_xyz = xy_to_xyz(p.b_x, p.b_y);
    let w_xyz = xy_to_xyz(p.w_x, p.w_y);

    // M = [Rx Gx Bx; Ry Gy By; Rz Gz Bz]
    let m = [
        [r_xyz[0], g_xyz[0], b_xyz[0]],
        [r_xyz[1], g_xyz[1], b_xyz[1]],
        [r_xyz[2], g_xyz[2], b_xyz[2]],
    ];

    let m_inv = invert_3x3_f64(m);

    // S = M^-1 * W
    let s = [
        m_inv[0][0] * w_xyz[0] + m_inv[0][1] * w_xyz[1] + m_inv[0][2] * w_xyz[2],
        m_inv[1][0] * w_xyz[0] + m_inv[1][1] * w_xyz[1] + m_inv[1][2] * w_xyz[2],
        m_inv[2][0] * w_xyz[0] + m_inv[2][1] * w_xyz[1] + m_inv[2][2] * w_xyz[2],
    ];

    // Result = M * diag(S)
    [
        [m[0][0] * s[0], m[0][1] * s[1], m[0][2] * s[2]],
        [m[1][0] * s[0], m[1][1] * s[1], m[1][2] * s[2]],
        [m[2][0] * s[0], m[2][1] * s[1], m[2][2] * s[2]],
    ]
}

fn xy_to_xyz(x: f64, y: f64) -> [f64; 3] {
    if y == 0.0 {
        return [0.0, 0.0, 0.0];
    }
    [x / y, 1.0, (1.0 - x - y) / y]
}

fn invert_3x3_f64(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);

    let inv_det = 1.0 / det;

    [
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv_det,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv_det,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv_det,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv_det,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv_det,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv_det,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv_det,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv_det,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv_det,
        ],
    ]
}

fn mul_3x3_f64(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut r = [[0f64; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    r
}

fn mul_3x3_f64_to_f32(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f32; 3]; 3] {
    let r = mul_3x3_f64(a, b);
    [
        [r[0][0] as f32, r[0][1] as f32, r[0][2] as f32],
        [r[1][0] as f32, r[1][1] as f32, r[1][2] as f32],
        [r[2][0] as f32, r[2][1] as f32, r[2][2] as f32],
    ]
}

/// Generate an sRGB EOTF (degamma) lookup table for the CRTC DEGAMMA_LUT property.
///
/// Each entry maps a normalized sRGB value to linear light using the sRGB transfer function.
/// Output format: `[red, green, blue, reserved]` with values in `[0, 65535]`.
pub fn generate_srgb_degamma_lut(size: u32) -> Vec<[u16; 4]> {
    (0..size)
        .map(|i| {
            let v = i as f64 / (size - 1) as f64;
            let linear = if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            };
            let val = (linear * 65535.0).round() as u16;
            [val, val, val, 0]
        })
        .collect()
}

/// Generate a PQ OETF (gamma) lookup table for the CRTC GAMMA_LUT property.
///
/// Maps linear light `[0, 1]` where 1.0 = `reference_luminance` cd/m² to PQ code values.
/// Output format: `[red, green, blue, reserved]` with values in `[0, 65535]`.
pub fn generate_pq_gamma_lut(size: u32, reference_luminance: f32) -> Vec<[u16; 4]> {
    // PQ (SMPTE ST 2084) constants.
    const M1: f64 = 0.1593017578125;
    const M2: f64 = 78.84375;
    const C1: f64 = 0.8359375;
    const C2: f64 = 18.8515625;
    const C3: f64 = 18.6875;

    (0..size)
        .map(|i| {
            let linear = i as f64 / (size - 1) as f64;
            // Normalize to absolute luminance: linear 1.0 = reference_luminance cd/m².
            let y = (linear * reference_luminance as f64 / 10000.0).max(0.0);
            let ym = y.powf(M1);
            let e = ((C1 + C2 * ym) / (1.0 + C3 * ym)).powf(M2);
            let val = (e * 65535.0).round().min(65535.0) as u16;
            [val, val, val, 0]
        })
        .collect()
}

/// Convert a 3×3 f32 matrix (row-major) to DRM `drm_color_ctm` S31.32 fixed-point format.
///
/// Each value is encoded as a u64 with the sign bit in the MSB (bit 63),
/// 31 integer bits, and 32 fractional bits. Row-major layout matches the
/// kernel's `drm_color_ctm.matrix[9]`.
pub fn matrix_to_drm_ctm(matrix: &[[f32; 3]; 3]) -> [u64; 9] {
    let mut result = [0u64; 9];
    for row in 0..3 {
        for col in 0..3 {
            let val = matrix[row][col] as f64;
            let abs = val.abs();
            // S31.32: 32 fractional bits.
            let fixed = (abs * (1u64 << 32) as f64).round() as u64;
            // Sign bit in MSB.
            let sign = if val < 0.0 { 1u64 << 63 } else { 0 };
            result[row * 3 + col] = sign | fixed;
        }
    }
    result
}

/// Extract color information from a parsed EDID using libdisplay-info.
pub fn edid_color_info(info: &libdisplay_info::info::Info) -> Option<EdidColorInfo> {
    let primaries = info.default_color_primaries();

    if !primaries.has_primaries || !primaries.has_default_white_point {
        return None;
    }

    // primary[0] = red, primary[1] = green, primary[2] = blue
    Some(EdidColorInfo {
        red: (
            primaries.primary[0].x as f64,
            primaries.primary[0].y as f64,
        ),
        green: (
            primaries.primary[1].x as f64,
            primaries.primary[1].y as f64,
        ),
        blue: (
            primaries.primary[2].x as f64,
            primaries.primary[2].y as f64,
        ),
        white: (
            primaries.default_white.x as f64,
            primaries.default_white.y as f64,
        ),
    })
}
