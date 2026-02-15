use std::cell::RefCell;

use anyhow::Context as _;
use glam::Mat3;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
    UniformType, UniformValue,
};
use smithay::backend::renderer::Offscreen;
use smithay::utils::{Physical, Size, Transform};

use super::renderer::NiriRenderer;
use super::shader_element::ShaderProgram;

pub struct Shaders {
    pub border: Option<ShaderProgram>,
    pub shadow: Option<ShaderProgram>,
    pub clipped_surface: Option<GlesTexProgram>,
    pub resize: Option<ShaderProgram>,
    pub gradient_fade: Option<GlesTexProgram>,
    pub custom_resize: RefCell<Option<ShaderProgram>>,
    pub custom_close: RefCell<Option<ShaderProgram>>,
    pub custom_open: RefCell<Option<ShaderProgram>>,
    /// Color correction program (GLES 3.0, compiled manually for sampler3D support).
    pub color_correction: Option<ColorCorrectionProgram>,
    /// Tone mapping program (GLES 3.0).
    pub tone_map: Option<ToneMapProgram>,
}

/// Raw GL program for color correction using a 3D LUT.
#[derive(Debug, Clone)]
pub struct ColorCorrectionProgram {
    pub program: u32,
    pub u_tex: i32,
    pub u_color_lut: i32,
    pub u_alpha: i32,
}

/// Raw GL program for tone mapping between transfer functions.
#[derive(Debug, Clone)]
pub struct ToneMapProgram {
    pub program: u32,
    pub u_tex: i32,
    pub u_src_tf: i32,
    pub u_dst_tf: i32,
    pub u_src_max_lum: i32,
    pub u_dst_max_lum: i32,
    pub u_color_matrix: i32,
}

#[derive(Debug, Clone, Copy)]
pub enum ProgramType {
    Border,
    Shadow,
    Resize,
    Close,
    Open,
}

impl Shaders {
    fn compile(renderer: &mut GlesRenderer) -> Self {
        let _span = tracy_client::span!("Shaders::compile");

        let border = ShaderProgram::compile(
            renderer,
            include_str!("border.frag"),
            &[
                UniformName::new("colorspace", UniformType::_1f),
                UniformName::new("hue_interpolation", UniformType::_1f),
                UniformName::new("color_from", UniformType::_4f),
                UniformName::new("color_to", UniformType::_4f),
                UniformName::new("grad_offset", UniformType::_2f),
                UniformName::new("grad_width", UniformType::_1f),
                UniformName::new("grad_vec", UniformType::_2f),
                UniformName::new("input_to_geo", UniformType::Matrix3x3),
                UniformName::new("geo_size", UniformType::_2f),
                UniformName::new("outer_radius", UniformType::_4f),
                UniformName::new("border_width", UniformType::_1f),
            ],
            &[],
        )
        .map_err(|err| {
            warn!("error compiling border shader: {err:?}");
        })
        .ok();

        let shadow = ShaderProgram::compile(
            renderer,
            include_str!("shadow.frag"),
            &[
                UniformName::new("shadow_color", UniformType::_4f),
                UniformName::new("sigma", UniformType::_1f),
                UniformName::new("input_to_geo", UniformType::Matrix3x3),
                UniformName::new("geo_size", UniformType::_2f),
                UniformName::new("corner_radius", UniformType::_4f),
                UniformName::new("window_input_to_geo", UniformType::Matrix3x3),
                UniformName::new("window_geo_size", UniformType::_2f),
                UniformName::new("window_corner_radius", UniformType::_4f),
            ],
            &[],
        )
        .map_err(|err| {
            warn!("error compiling shadow shader: {err:?}");
        })
        .ok();

        let clipped_surface = renderer
            .compile_custom_texture_shader(
                include_str!("clipped_surface.frag"),
                &[
                    UniformName::new("niri_scale", UniformType::_1f),
                    UniformName::new("geo_size", UniformType::_2f),
                    UniformName::new("corner_radius", UniformType::_4f),
                    UniformName::new("input_to_geo", UniformType::Matrix3x3),
                ],
            )
            .map_err(|err| {
                warn!("error compiling clipped surface shader: {err:?}");
            })
            .ok();

        let resize = compile_resize_program(renderer, include_str!("resize.frag"))
            .map_err(|err| {
                warn!("error compiling resize shader: {err:?}");
            })
            .ok();

        let gradient_fade = renderer
            .compile_custom_texture_shader(
                include_str!("gradient_fade.frag"),
                &[UniformName::new("cutoff", UniformType::_2f)],
            )
            .map_err(|err| {
                warn!("error compiling gradient fade shader: {err:?}");
            })
            .ok();

        let color_correction = compile_gles3_program(
            renderer,
            FULLSCREEN_VERT_300ES,
            include_str!("color_correction.frag"),
            &["tex", "color_lut", "alpha"],
        )
        .map(|p| ColorCorrectionProgram {
            program: p.0,
            u_tex: p.1[0],
            u_color_lut: p.1[1],
            u_alpha: p.1[2],
        })
        .map_err(|err| {
            warn!("error compiling color correction shader: {err}");
        })
        .ok();

        let tone_map = compile_gles3_program(
            renderer,
            FULLSCREEN_VERT_300ES,
            include_str!("tone_map.frag"),
            &["tex", "src_tf", "dst_tf", "src_max_lum", "dst_max_lum", "color_matrix"],
        )
        .map(|p| ToneMapProgram {
            program: p.0,
            u_tex: p.1[0],
            u_src_tf: p.1[1],
            u_dst_tf: p.1[2],
            u_src_max_lum: p.1[3],
            u_dst_max_lum: p.1[4],
            u_color_matrix: p.1[5],
        })
        .map_err(|err| {
            warn!("error compiling tone map shader: {err}");
        })
        .ok();

        Self {
            border,
            shadow,
            clipped_surface,
            resize,
            gradient_fade,
            custom_resize: RefCell::new(None),
            custom_close: RefCell::new(None),
            custom_open: RefCell::new(None),
            color_correction,
            tone_map,
        }
    }

    pub fn get_from_frame<'a>(frame: &'a mut GlesFrame<'_, '_>) -> &'a Self {
        let data = frame.egl_context().user_data();
        data.get()
            .expect("shaders::init() must be called when creating the renderer")
    }

    pub fn get(renderer: &mut impl NiriRenderer) -> &Self {
        let renderer = renderer.as_gles_renderer();
        let data = renderer.egl_context().user_data();
        data.get()
            .expect("shaders::init() must be called when creating the renderer")
    }

    pub fn replace_custom_resize_program(
        &self,
        program: Option<ShaderProgram>,
    ) -> Option<ShaderProgram> {
        self.custom_resize.replace(program)
    }

    pub fn replace_custom_close_program(
        &self,
        program: Option<ShaderProgram>,
    ) -> Option<ShaderProgram> {
        self.custom_close.replace(program)
    }

    pub fn replace_custom_open_program(
        &self,
        program: Option<ShaderProgram>,
    ) -> Option<ShaderProgram> {
        self.custom_open.replace(program)
    }

    pub fn program(&self, program: ProgramType) -> Option<ShaderProgram> {
        match program {
            ProgramType::Border => self.border.clone(),
            ProgramType::Shadow => self.shadow.clone(),
            ProgramType::Resize => self
                .custom_resize
                .borrow()
                .clone()
                .or_else(|| self.resize.clone()),
            ProgramType::Close => self.custom_close.borrow().clone(),
            ProgramType::Open => self.custom_open.borrow().clone(),
        }
    }
}

pub fn init(renderer: &mut GlesRenderer) {
    let shaders = Shaders::compile(renderer);
    let data = renderer.egl_context().user_data();
    if !data.insert_if_missing(|| shaders) {
        error!("shaders were already compiled");
    }
}

fn compile_resize_program(
    renderer: &mut GlesRenderer,
    src: &str,
) -> Result<ShaderProgram, GlesError> {
    let mut program = include_str!("resize_prelude.frag").to_string();
    program.push_str(src);
    program.push_str(include_str!("resize_epilogue.frag"));

    ShaderProgram::compile(
        renderer,
        &program,
        &[
            UniformName::new("niri_input_to_curr_geo", UniformType::Matrix3x3),
            UniformName::new("niri_curr_geo_to_prev_geo", UniformType::Matrix3x3),
            UniformName::new("niri_curr_geo_to_next_geo", UniformType::Matrix3x3),
            UniformName::new("niri_curr_geo_size", UniformType::_2f),
            UniformName::new("niri_geo_to_tex_prev", UniformType::Matrix3x3),
            UniformName::new("niri_geo_to_tex_next", UniformType::Matrix3x3),
            UniformName::new("niri_progress", UniformType::_1f),
            UniformName::new("niri_clamped_progress", UniformType::_1f),
            UniformName::new("niri_corner_radius", UniformType::_4f),
            UniformName::new("niri_clip_to_geometry", UniformType::_1f),
        ],
        &["niri_tex_prev", "niri_tex_next"],
    )
}

pub fn set_custom_resize_program(renderer: &mut GlesRenderer, src: Option<&str>) {
    let program = if let Some(src) = src {
        match compile_resize_program(renderer, src) {
            Ok(program) => Some(program),
            Err(err) => {
                warn!("error compiling custom resize shader: {err:?}");
                return;
            }
        }
    } else {
        None
    };

    if let Some(prev) = Shaders::get(renderer).replace_custom_resize_program(program) {
        if let Err(err) = prev.destroy(renderer) {
            warn!("error destroying previous custom resize shader: {err:?}");
        }
    }
}

fn compile_close_program(
    renderer: &mut GlesRenderer,
    src: &str,
) -> Result<ShaderProgram, GlesError> {
    let mut program = include_str!("close_prelude.frag").to_string();
    program.push_str(src);
    program.push_str(include_str!("close_epilogue.frag"));

    ShaderProgram::compile(
        renderer,
        &program,
        &[
            UniformName::new("niri_input_to_geo", UniformType::Matrix3x3),
            UniformName::new("niri_geo_size", UniformType::_2f),
            UniformName::new("niri_geo_to_tex", UniformType::Matrix3x3),
            UniformName::new("niri_progress", UniformType::_1f),
            UniformName::new("niri_clamped_progress", UniformType::_1f),
            UniformName::new("niri_random_seed", UniformType::_1f),
        ],
        &["niri_tex"],
    )
}

pub fn set_custom_close_program(renderer: &mut GlesRenderer, src: Option<&str>) {
    let program = if let Some(src) = src {
        match compile_close_program(renderer, src) {
            Ok(program) => Some(program),
            Err(err) => {
                warn!("error compiling custom close shader: {err:?}");
                return;
            }
        }
    } else {
        None
    };

    if let Some(prev) = Shaders::get(renderer).replace_custom_close_program(program) {
        if let Err(err) = prev.destroy(renderer) {
            warn!("error destroying previous custom close shader: {err:?}");
        }
    }
}

fn compile_open_program(
    renderer: &mut GlesRenderer,
    src: &str,
) -> Result<ShaderProgram, GlesError> {
    let mut program = include_str!("open_prelude.frag").to_string();
    program.push_str(src);
    program.push_str(include_str!("open_epilogue.frag"));

    ShaderProgram::compile(
        renderer,
        &program,
        &[
            UniformName::new("niri_input_to_geo", UniformType::Matrix3x3),
            UniformName::new("niri_geo_size", UniformType::_2f),
            UniformName::new("niri_geo_to_tex", UniformType::Matrix3x3),
            UniformName::new("niri_progress", UniformType::_1f),
            UniformName::new("niri_clamped_progress", UniformType::_1f),
            UniformName::new("niri_random_seed", UniformType::_1f),
        ],
        &["niri_tex"],
    )
}

pub fn set_custom_open_program(renderer: &mut GlesRenderer, src: Option<&str>) {
    let program = if let Some(src) = src {
        match compile_open_program(renderer, src) {
            Ok(program) => Some(program),
            Err(err) => {
                warn!("error compiling custom open shader: {err:?}");
                return;
            }
        }
    } else {
        None
    };

    if let Some(prev) = Shaders::get(renderer).replace_custom_open_program(program) {
        if let Err(err) = prev.destroy(renderer) {
            warn!("error destroying previous custom open shader: {err:?}");
        }
    }
}

/// Fullscreen triangle vertex shader for GLES 3.0 post-processing passes.
const FULLSCREEN_VERT_300ES: &str = r#"#version 300 es
precision highp float;
out vec2 v_coords;
void main() {
    // Fullscreen triangle: vertices 0,1,2 cover the screen.
    float x = float((gl_VertexID & 1) << 2) - 1.0;
    float y = float((gl_VertexID & 2) << 1) - 1.0;
    v_coords = vec2(x * 0.5 + 0.5, y * 0.5 + 0.5);
    gl_Position = vec4(x, y, 0.0, 1.0);
}
"#;

/// Compile a GLES 3.0 shader program from vertex + fragment source using raw GL.
/// Returns (program, uniform_locations) on success.
fn compile_gles3_program(
    renderer: &mut GlesRenderer,
    vert_src: &str,
    frag_src: &str,
    uniform_names: &[&str],
) -> anyhow::Result<(u32, Vec<i32>)> {
    use smithay::backend::renderer::gles::ffi;
    use std::ffi::CString;

    renderer
        .with_context(|gl| unsafe {
            let compile_shader = |shader_type: u32, src: &str| -> anyhow::Result<u32> {
                let shader = gl.CreateShader(shader_type);
                let c_src = CString::new(src).unwrap();
                let ptr = c_src.as_ptr();
                gl.ShaderSource(shader, 1, &ptr, std::ptr::null());
                gl.CompileShader(shader);

                let mut success = 0i32;
                gl.GetShaderiv(shader, ffi::COMPILE_STATUS, &mut success);
                if success == 0 {
                    let mut len = 0i32;
                    gl.GetShaderiv(shader, ffi::INFO_LOG_LENGTH, &mut len);
                    let mut buf = vec![0u8; len as usize];
                    gl.GetShaderInfoLog(shader, len, std::ptr::null_mut(), buf.as_mut_ptr() as *mut _);
                    let log = String::from_utf8_lossy(&buf);
                    gl.DeleteShader(shader);
                    anyhow::bail!("shader compilation failed: {log}");
                }
                Ok(shader)
            };

            let vs = compile_shader(ffi::VERTEX_SHADER, vert_src)?;
            let fs = compile_shader(ffi::FRAGMENT_SHADER, frag_src)?;

            let program = gl.CreateProgram();
            gl.AttachShader(program, vs);
            gl.AttachShader(program, fs);
            gl.LinkProgram(program);

            let mut success = 0i32;
            gl.GetProgramiv(program, ffi::LINK_STATUS, &mut success);
            if success == 0 {
                let mut len = 0i32;
                gl.GetProgramiv(program, ffi::INFO_LOG_LENGTH, &mut len);
                let mut buf = vec![0u8; len as usize];
                gl.GetProgramInfoLog(program, len, std::ptr::null_mut(), buf.as_mut_ptr() as *mut _);
                let log = String::from_utf8_lossy(&buf);
                gl.DeleteProgram(program);
                gl.DeleteShader(vs);
                gl.DeleteShader(fs);
                anyhow::bail!("shader linking failed: {log}");
            }

            gl.DeleteShader(vs);
            gl.DeleteShader(fs);

            let locations: Vec<i32> = uniform_names
                .iter()
                .map(|name| {
                    let c_name = CString::new(*name).unwrap();
                    gl.GetUniformLocation(program, c_name.as_ptr())
                })
                .collect();

            Ok((program, locations))
        })
        .context("failed to access GL context")?
}

pub fn mat3_uniform(name: &str, mat: Mat3) -> Uniform<'_> {
    Uniform::new(
        name,
        UniformValue::Matrix3x3 {
            matrices: vec![mat.to_cols_array()],
            transpose: false,
        },
    )
}

/// Apply tone mapping to an input texture, producing a new output texture.
///
/// Converts between transfer functions (e.g. sRGB→PQ) with optional gamut conversion.
/// Transfer function IDs: 0=sRGB, 1=PQ, 2=HLG, 3=Linear.
pub fn apply_tone_map(
    renderer: &mut GlesRenderer,
    input_texture: &GlesTexture,
    output_size: Size<i32, Physical>,
    src_tf: i32,
    dst_tf: i32,
    src_max_lum: f32,
    dst_max_lum: f32,
    color_matrix: Mat3,
) -> anyhow::Result<GlesTexture> {
    use smithay::backend::renderer::gles::ffi;

    let tone_map = Shaders::get(renderer)
        .tone_map
        .clone()
        .context("tone map shader not compiled")?;

    let buffer_size = output_size.to_logical(1).to_buffer(1, Transform::Normal);
    let output_texture: GlesTexture = renderer
        .create_buffer(Fourcc::Abgr8888, buffer_size)
        .context("error creating tone map output texture")?;

    let input_tex_id = input_texture.tex_id();
    let output_tex_id = output_texture.tex_id();
    let w = output_size.w;
    let h = output_size.h;
    let mat_cols = color_matrix.to_cols_array();

    renderer
        .with_context(|gl| unsafe {
            // Save current FBO binding.
            let mut prev_fbo = 0i32;
            gl.GetIntegerv(ffi::FRAMEBUFFER_BINDING, &mut prev_fbo);

            // Create temporary FBO and attach output texture.
            let mut fbo = 0u32;
            gl.GenFramebuffers(1, &mut fbo);
            gl.BindFramebuffer(ffi::FRAMEBUFFER, fbo);
            gl.FramebufferTexture2D(
                ffi::FRAMEBUFFER,
                ffi::COLOR_ATTACHMENT0,
                ffi::TEXTURE_2D,
                output_tex_id,
                0,
            );

            // Set up rendering state.
            gl.Viewport(0, 0, w, h);
            gl.UseProgram(tone_map.program);
            gl.Disable(ffi::BLEND);

            // Bind input texture.
            gl.ActiveTexture(ffi::TEXTURE0);
            gl.BindTexture(ffi::TEXTURE_2D, input_tex_id);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);

            // Set uniforms.
            gl.Uniform1i(tone_map.u_tex, 0);
            gl.Uniform1i(tone_map.u_src_tf, src_tf);
            gl.Uniform1i(tone_map.u_dst_tf, dst_tf);
            gl.Uniform1f(tone_map.u_src_max_lum, src_max_lum);
            gl.Uniform1f(tone_map.u_dst_max_lum, dst_max_lum);
            gl.UniformMatrix3fv(tone_map.u_color_matrix, 1, ffi::FALSE, mat_cols.as_ptr());

            // Draw fullscreen triangle.
            gl.DrawArrays(ffi::TRIANGLES, 0, 3);

            // Cleanup.
            gl.Enable(ffi::BLEND);
            gl.BindFramebuffer(ffi::FRAMEBUFFER, prev_fbo as u32);
            gl.DeleteFramebuffers(1, &fbo);
        })
        .context("failed to access GL context for tone mapping")?;

    Ok(output_texture)
}
