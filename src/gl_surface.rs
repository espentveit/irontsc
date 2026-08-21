//! The session surface, rendered on the GPU.
//!
//! Presenting the desktop through a `GdkMemoryTexture` means every change, however small,
//! rebuilds and re-uploads the whole framebuffer: 16MB per frame at 2560x1600 for a one
//! character edit. Here the desktop lives in a single GL texture that stays on the GPU, and an
//! update only touches the rectangle that actually changed via `glTexSubImage2D`.
//!
//! This also removes the need to double-buffer the framebuffer on the CPU side, since GTK never
//! holds a reference to it.

use std::cell::RefCell;
use std::rc::Rc;

use glow::HasContext as _;
use gtk::prelude::*;

/// A rectangle of pixels waiting to be pushed into the texture on the next render.
struct PendingUpload {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

#[derive(Default)]
struct SurfaceState {
    gl: Option<Rc<glow::Context>>,
    program: Option<glow::Program>,
    vertex_array: Option<glow::VertexArray>,
    texture: Option<glow::Texture>,
    /// Size of the allocated texture, which may lag the requested size until the next render.
    texture_size: (u32, u32),
    surface_size: (u32, u32),
    pending: Vec<PendingUpload>,
}

#[derive(Clone)]
pub struct GlSurface {
    area: gtk::GLArea,
    state: Rc<RefCell<SurfaceState>>,
}

const VERTEX_SHADER: &str = r#"#version 150 core
in vec2 position;
in vec2 texcoord;
out vec2 uv;
void main() {
    uv = texcoord;
    gl_Position = vec4(position, 0.0, 1.0);
}
"#;

// The framebuffer is BGRA, which is not a portable upload format, so it is uploaded as RGBA and
// swizzled here instead.
const FRAGMENT_SHADER: &str = r#"#version 150 core
in vec2 uv;
out vec4 color;
uniform sampler2D surface;
void main() {
    color = texture(surface, uv).bgra;
}
"#;

/// Two triangles covering the viewport. The texture rows run top-down while GL's origin is at
/// the bottom, so the vertical texture coordinate is flipped here.
const VERTICES: [f32; 16] = [
    -1.0, -1.0, 0.0, 1.0, // bottom left
    1.0, -1.0, 1.0, 1.0, // bottom right
    -1.0, 1.0, 0.0, 0.0, // top left
    1.0, 1.0, 1.0, 0.0, // top right
];

impl GlSurface {
    pub fn new() -> Self {
        let area = gtk::GLArea::new();
        area.set_hexpand(true);
        area.set_vexpand(true);
        area.set_has_depth_buffer(false);
        area.set_has_stencil_buffer(false);
        // Desktop GL only, so the shaders above can rely on #version 150.
        area.set_allowed_apis(gtk::gdk::GLAPI::GL);

        let surface = Self {
            area,
            state: Rc::new(RefCell::new(SurfaceState::default())),
        };

        let state = surface.state.clone();
        surface.area.connect_realize(move |area| {
            area.make_current();
            if let Some(error) = area.error() {
                tracing::error!(%error, "GLArea failed to realize");
                return;
            }
            if let Err(error) = SurfaceState::initialise(&state) {
                tracing::error!(error = format!("{error:#}"), "GL surface setup failed");
            }
        });

        let state = surface.state.clone();
        surface.area.connect_render(move |_, _| {
            SurfaceState::render(&state);
            gtk::glib::Propagation::Stop
        });

        let state = surface.state.clone();
        surface.area.connect_unrealize(move |area| {
            area.make_current();
            SurfaceState::teardown(&state);
        });

        surface
    }

    pub fn widget(&self) -> &gtk::GLArea {
        &self.area
    }

    /// Declares the size of the remote surface. The texture is (re)allocated on the next render.
    pub fn resize(&self, width: u32, height: u32) {
        let mut state = self.state.borrow_mut();
        if state.surface_size == (width, height) {
            return;
        }
        state.surface_size = (width, height);
        state.pending.clear();
        drop(state);
        self.area.queue_render();
    }

    /// Queues a rectangle of BGRA pixels to be pushed into the texture.
    pub fn update_region(&self, x: u32, y: u32, width: u32, height: u32, pixels: Vec<u8>) {
        if width == 0 || height == 0 {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            state.pending.push(PendingUpload {
                x,
                y,
                width,
                height,
                pixels,
            });
        }
        self.area.queue_render();
    }

    pub fn clear(&self) {
        let mut state = self.state.borrow_mut();
        state.surface_size = (0, 0);
        state.pending.clear();
        drop(state);
        self.area.queue_render();
    }
}

impl SurfaceState {
    fn initialise(state: &Rc<RefCell<Self>>) -> anyhow::Result<()> {
        // GTK links libepoxy, which re-exports every GL entry point, so the loader can come
        // straight from it.
        let library = unsafe { libloading::Library::new("libepoxy.so.0") }
            .or_else(|_| unsafe { libloading::Library::new("libepoxy.so") })
            .map_err(|e| anyhow::anyhow!("failed to open libepoxy: {e}"))?;

        let gl = unsafe {
            glow::Context::from_loader_function(|symbol| {
                // libepoxy does not export `glFoo` as a function. It exports `epoxy_glFoo`, a
                // variable holding the function pointer, which starts out pointing at a resolver
                // stub and is safe to call. So look up the prefixed name and read the pointer
                // out of it rather than treating the symbol address as the entry point.
                let mut name = Vec::with_capacity(symbol.len() + 8);
                name.extend_from_slice(b"epoxy_");
                name.extend_from_slice(symbol.as_bytes());
                name.push(0);

                match library.get::<*const std::ffi::c_void>(&name) {
                    Ok(variable) => {
                        let slot = *variable as *const *const std::ffi::c_void;
                        if slot.is_null() {
                            std::ptr::null()
                        } else {
                            *slot
                        }
                    }
                    Err(_) => std::ptr::null(),
                }
            })
        };

        // The library must outlive the loader-resolved pointers.
        std::mem::forget(library);

        let program = unsafe { Self::build_program(&gl)? };
        let vertex_array = unsafe { Self::build_geometry(&gl, program)? };

        let mut state = state.borrow_mut();
        state.program = Some(program);
        state.vertex_array = Some(vertex_array);
        state.gl = Some(Rc::new(gl));
        tracing::info!("🖥️  GL session surface initialised");
        Ok(())
    }

    unsafe fn build_program(gl: &glow::Context) -> anyhow::Result<glow::Program> {
        let program = gl
            .create_program()
            .map_err(|e| anyhow::anyhow!("create_program: {e}"))?;

        for (kind, source) in [
            (glow::VERTEX_SHADER, VERTEX_SHADER),
            (glow::FRAGMENT_SHADER, FRAGMENT_SHADER),
        ] {
            let shader = gl
                .create_shader(kind)
                .map_err(|e| anyhow::anyhow!("create_shader: {e}"))?;
            gl.shader_source(shader, source);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                anyhow::bail!("shader compilation failed: {}", gl.get_shader_info_log(shader));
            }
            gl.attach_shader(program, shader);
            gl.delete_shader(shader);
        }

        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            anyhow::bail!("program link failed: {}", gl.get_program_info_log(program));
        }
        Ok(program)
    }

    unsafe fn build_geometry(
        gl: &glow::Context,
        program: glow::Program,
    ) -> anyhow::Result<glow::VertexArray> {
        let vertex_array = gl
            .create_vertex_array()
            .map_err(|e| anyhow::anyhow!("create_vertex_array: {e}"))?;
        gl.bind_vertex_array(Some(vertex_array));

        let buffer = gl
            .create_buffer()
            .map_err(|e| anyhow::anyhow!("create_buffer: {e}"))?;
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(buffer));
        gl.buffer_data_u8_slice(
            glow::ARRAY_BUFFER,
            std::slice::from_raw_parts(
                VERTICES.as_ptr() as *const u8,
                std::mem::size_of_val(&VERTICES),
            ),
            glow::STATIC_DRAW,
        );

        let stride = 4 * std::mem::size_of::<f32>() as i32;
        let position = gl.get_attrib_location(program, "position").unwrap_or(0);
        gl.enable_vertex_attrib_array(position);
        gl.vertex_attrib_pointer_f32(position, 2, glow::FLOAT, false, stride, 0);

        let texcoord = gl.get_attrib_location(program, "texcoord").unwrap_or(1);
        gl.enable_vertex_attrib_array(texcoord);
        gl.vertex_attrib_pointer_f32(
            texcoord,
            2,
            glow::FLOAT,
            false,
            stride,
            2 * std::mem::size_of::<f32>() as i32,
        );

        gl.bind_vertex_array(None);
        Ok(vertex_array)
    }

    fn render(state: &Rc<RefCell<Self>>) {
        let mut state = state.borrow_mut();
        let Some(gl) = state.gl.clone() else {
            return;
        };

        unsafe {
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }

        let (width, height) = state.surface_size;
        if width == 0 || height == 0 {
            return;
        }

        // Allocate or reallocate the texture when the surface size changes.
        if state.texture.is_none() || state.texture_size != (width, height) {
            unsafe {
                if let Some(old) = state.texture.take() {
                    gl.delete_texture(old);
                }
                let Ok(texture) = gl.create_texture() else {
                    tracing::error!("failed to create GL texture for the session surface");
                    return;
                };
                gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MIN_FILTER,
                    glow::LINEAR as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MAG_FILTER,
                    glow::LINEAR as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_WRAP_S,
                    glow::CLAMP_TO_EDGE as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_WRAP_T,
                    glow::CLAMP_TO_EDGE as i32,
                );
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    width as i32,
                    height as i32,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(None),
                );
                state.texture = Some(texture);
                state.texture_size = (width, height);
            }
        }

        let texture = state.texture;
        let pending = std::mem::take(&mut state.pending);

        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, texture);
            for upload in pending {
                // Skip anything that no longer fits, which can happen if a resize raced an
                // update that was already queued.
                if upload.x + upload.width > width || upload.y + upload.height > height {
                    continue;
                }
                gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    upload.x as i32,
                    upload.y as i32,
                    upload.width as i32,
                    upload.height as i32,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(&upload.pixels)),
                );
            }

            if let (Some(program), Some(vertex_array)) = (state.program, state.vertex_array) {
                gl.use_program(Some(program));
                if let Some(location) = gl.get_uniform_location(program, "surface") {
                    gl.uniform_1_i32(Some(&location), 0);
                }
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, texture);
                gl.bind_vertex_array(Some(vertex_array));
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                gl.bind_vertex_array(None);
            }
        }
    }

    fn teardown(state: &Rc<RefCell<Self>>) {
        let mut state = state.borrow_mut();
        let Some(gl) = state.gl.clone() else {
            return;
        };
        unsafe {
            if let Some(texture) = state.texture.take() {
                gl.delete_texture(texture);
            }
            if let Some(vertex_array) = state.vertex_array.take() {
                gl.delete_vertex_array(vertex_array);
            }
            if let Some(program) = state.program.take() {
                gl.delete_program(program);
            }
        }
        state.gl = None;
        state.texture_size = (0, 0);
    }
}
