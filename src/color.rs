use std::path::Path;

use anyhow::Context as _;

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
