//! The small subset of legacy (OpenGL 1.1, fixed-function) GL used to draw
//! ImGui inside XPLM windows.
//!
//! Every X-Plane 12 platform exports these entry points directly from the
//! system GL library, so no function loader is needed. They also work in
//! the legacy 2.1 context X-Plane gives plugins on macOS, where modern GLSL
//! 3.30 shaders are unavailable.

use std::ffi::c_void;

use imgui::DrawVert;

use crate::sys;

type GLenum = u32;

const GL_MODELVIEW: GLenum = 0x1700;
const GL_MATRIX_MODE: GLenum = 0x0BA0;
const GL_MODELVIEW_MATRIX: GLenum = 0x0BA6;
const GL_PROJECTION_MATRIX: GLenum = 0x0BA7;
const GL_VIEWPORT: GLenum = 0x0BA2;
const GL_SCISSOR_TEST: GLenum = 0x0C11;
const GL_SCISSOR_BOX: GLenum = 0x0C10;
const GL_SCISSOR_BIT: u32 = 0x0008_0000;
const GL_CLIENT_VERTEX_ARRAY_BIT: u32 = 0x0000_0002;
const GL_VERTEX_ARRAY: GLenum = 0x8074;
const GL_COLOR_ARRAY: GLenum = 0x8076;
const GL_TEXTURE_COORD_ARRAY: GLenum = 0x8078;
const GL_FLOAT: GLenum = 0x1406;
const GL_UNSIGNED_BYTE: GLenum = 0x1401;
const GL_UNSIGNED_SHORT: GLenum = 0x1403;
const GL_TRIANGLES: GLenum = 0x0004;
const GL_TEXTURE_2D: GLenum = 0x0DE1;
const GL_TEXTURE_MIN_FILTER: GLenum = 0x2801;
const GL_TEXTURE_MAG_FILTER: GLenum = 0x2800;
const GL_TEXTURE_WRAP_S: GLenum = 0x2802;
const GL_TEXTURE_WRAP_T: GLenum = 0x2803;
const GL_CLAMP_TO_EDGE: i32 = 0x812F;
const GL_LINEAR: i32 = 0x2601;
const GL_RGBA: GLenum = 0x1908;
const GL_UNPACK_ROW_LENGTH: GLenum = 0x0CF2;
const GL_UNPACK_ALIGNMENT: GLenum = 0x0CF5;

#[cfg_attr(target_os = "windows", link(name = "opengl32"))]
#[cfg_attr(target_os = "macos", link(name = "OpenGL", kind = "framework"))]
#[cfg_attr(target_os = "linux", link(name = "GL"))]
unsafe extern "system" {
    fn glMatrixMode(mode: GLenum);
    fn glPushMatrix();
    fn glPopMatrix();
    fn glTranslatef(x: f32, y: f32, z: f32);
    fn glScalef(x: f32, y: f32, z: f32);
    fn glGetFloatv(pname: GLenum, params: *mut f32);
    fn glGetIntegerv(pname: GLenum, params: *mut i32);
    fn glIsEnabled(cap: GLenum) -> u8;
    fn glEnable(cap: GLenum);
    fn glScissor(x: i32, y: i32, width: i32, height: i32);
    fn glPushAttrib(mask: u32);
    fn glPopAttrib();
    fn glPushClientAttrib(mask: u32);
    fn glPopClientAttrib();
    fn glEnableClientState(array: GLenum);
    fn glVertexPointer(size: i32, ty: GLenum, stride: i32, pointer: *const c_void);
    fn glTexCoordPointer(size: i32, ty: GLenum, stride: i32, pointer: *const c_void);
    fn glColorPointer(size: i32, ty: GLenum, stride: i32, pointer: *const c_void);
    fn glDrawElements(mode: GLenum, count: i32, ty: GLenum, indices: *const c_void);
    fn glTexParameteri(target: GLenum, pname: GLenum, param: i32);
    fn glPixelStorei(pname: GLenum, param: i32);
    #[allow(clippy::too_many_arguments)]
    fn glTexImage2D(
        target: GLenum,
        level: i32,
        internal_format: i32,
        width: i32,
        height: i32,
        border: i32,
        format: GLenum,
        ty: GLenum,
        pixels: *const c_void,
    );
    fn glDeleteTextures(n: i32, textures: *const u32);
}

/// An RGBA texture owned by the plugin. Deleted on drop.
pub struct Texture {
    id: i32,
}

impl Texture {
    /// Uploads tightly packed RGBA8 pixels.
    pub fn from_rgba(width: u32, height: u32, pixels: &[u8]) -> Self {
        assert_eq!(pixels.len(), width as usize * height as usize * 4);
        let mut id = 0;
        unsafe {
            sys::XPLMGenerateTextureNumbers(&mut id, 1);
            sys::XPLMBindTexture2d(id, 0);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
            glPixelStorei(GL_UNPACK_ROW_LENGTH, 0);
            glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
            glTexImage2D(
                GL_TEXTURE_2D,
                0,
                GL_RGBA as i32,
                width as i32,
                height as i32,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                pixels.as_ptr() as *const c_void,
            );
        }
        Self { id }
    }

    pub fn id(&self) -> i32 {
        self.id
    }
}

impl Drop for Texture {
    fn drop(&mut self) {
        let id = self.id as u32;
        unsafe { glDeleteTextures(1, &id) }
    }
}

/// Drawing scope for 2D content whose origin is at window coordinates
/// (`left`, `top`) with y pointing down, as ImGui produces it. Saves and
/// restores every piece of GL state it touches that XPLM does not track.
pub struct Scope2d {
    /// Column-major projection * modelview, mapping local coords to clip space.
    mvp: [f32; 16],
    viewport: [i32; 4],
    /// Scissor box that was active before this scope, if any.
    outer_scissor: Option<[i32; 4]>,
    previous_matrix_mode: i32,
}

impl Scope2d {
    pub fn begin(left: f32, top: f32) -> Self {
        unsafe {
            // Fog off, 1 texture unit, lighting off, alpha test off,
            // blending on, depth test and writes off.
            sys::XPLMSetGraphicsState(0, 1, 0, 0, 1, 0, 0);

            let mut previous_matrix_mode = 0;
            glGetIntegerv(GL_MATRIX_MODE, &mut previous_matrix_mode);
            glMatrixMode(GL_MODELVIEW);
            glPushMatrix();
            glTranslatef(left, top, 0.0);
            glScalef(1.0, -1.0, 1.0);

            let mut modelview = [0.0f32; 16];
            let mut projection = [0.0f32; 16];
            let mut viewport = [0i32; 4];
            glGetFloatv(GL_MODELVIEW_MATRIX, modelview.as_mut_ptr());
            glGetFloatv(GL_PROJECTION_MATRIX, projection.as_mut_ptr());
            glGetIntegerv(GL_VIEWPORT, viewport.as_mut_ptr());

            let outer_scissor = if glIsEnabled(GL_SCISSOR_TEST) != 0 {
                let mut b = [0i32; 4];
                glGetIntegerv(GL_SCISSOR_BOX, b.as_mut_ptr());
                Some(b)
            } else {
                None
            };

            glPushAttrib(GL_SCISSOR_BIT);
            glPushClientAttrib(GL_CLIENT_VERTEX_ARRAY_BIT);
            glEnable(GL_SCISSOR_TEST);
            glEnableClientState(GL_VERTEX_ARRAY);
            glEnableClientState(GL_TEXTURE_COORD_ARRAY);
            glEnableClientState(GL_COLOR_ARRAY);

            Self {
                mvp: mat4_mul(&projection, &modelview),
                viewport,
                outer_scissor,
                previous_matrix_mode,
            }
        }
    }

    /// Restricts drawing to a local rectangle `[x0, y0, x1, y1]`.
    pub fn set_clip(&self, rect: [f32; 4]) {
        let box_ = clip_to_scissor(&self.mvp, self.viewport, rect, self.outer_scissor);
        unsafe { glScissor(box_[0], box_[1], box_[2], box_[3]) }
    }

    /// Draws indexed triangles with a texture (bind happens through XPLM so
    /// its texture cache stays correct).
    pub fn draw(&self, texture_id: i32, vertices: &[DrawVert], indices: &[u16]) {
        if indices.is_empty() {
            return;
        }
        let max_index = indices.iter().copied().max().unwrap_or(0) as usize;
        assert!(
            max_index < vertices.len(),
            "index out of range of vertex buffer"
        );
        const STRIDE: i32 = std::mem::size_of::<DrawVert>() as i32;
        let base = vertices.as_ptr() as *const u8;
        unsafe {
            sys::XPLMBindTexture2d(texture_id, 0);
            glVertexPointer(2, GL_FLOAT, STRIDE, base.add(0) as *const c_void);
            glTexCoordPointer(2, GL_FLOAT, STRIDE, base.add(8) as *const c_void);
            glColorPointer(4, GL_UNSIGNED_BYTE, STRIDE, base.add(16) as *const c_void);
            glDrawElements(
                GL_TRIANGLES,
                indices.len() as i32,
                GL_UNSIGNED_SHORT,
                indices.as_ptr() as *const c_void,
            );
        }
    }
}

impl Drop for Scope2d {
    fn drop(&mut self) {
        unsafe {
            glPopClientAttrib();
            glPopAttrib();
            glMatrixMode(GL_MODELVIEW);
            glPopMatrix();
            glMatrixMode(self.previous_matrix_mode as GLenum);
        }
    }
}

// The vertex pointers above assume ImGui's layout: pos, uv, col.
const _: () = assert!(std::mem::size_of::<DrawVert>() == 20);

/// Column-major 4x4 matrix product `a * b`.
fn mat4_mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut out = [0.0; 16];
    for col in 0..4 {
        for row in 0..4 {
            out[col * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[col * 4 + k]).sum();
        }
    }
    out
}

/// Maps a local point to window pixel coordinates through the full GL
/// transform (projection, modelview, viewport).
fn to_pixels(mvp: &[f32; 16], viewport: [i32; 4], x: f32, y: f32) -> [f32; 2] {
    let cx = mvp[0] * x + mvp[4] * y + mvp[12];
    let cy = mvp[1] * x + mvp[5] * y + mvp[13];
    let cw = mvp[3] * x + mvp[7] * y + mvp[15];
    let (nx, ny) = if cw != 0.0 {
        (cx / cw, cy / cw)
    } else {
        (cx, cy)
    };
    [
        viewport[0] as f32 + (nx + 1.0) * 0.5 * viewport[2] as f32,
        viewport[1] as f32 + (ny + 1.0) * 0.5 * viewport[3] as f32,
    ]
}

/// Converts a local clip rectangle to a GL scissor box `[x, y, w, h]`,
/// intersected with any scissor box that was already active.
fn clip_to_scissor(
    mvp: &[f32; 16],
    viewport: [i32; 4],
    rect: [f32; 4],
    outer: Option<[i32; 4]>,
) -> [i32; 4] {
    let a = to_pixels(mvp, viewport, rect[0], rect[1]);
    let b = to_pixels(mvp, viewport, rect[2], rect[3]);
    // Round rather than floor/ceil: the transform yields near-integers with
    // float error (199.99998), and fractional UI scales land on half pixels.
    let mut x0 = a[0].min(b[0]).round() as i32;
    let mut y0 = a[1].min(b[1]).round() as i32;
    let mut x1 = a[0].max(b[0]).round() as i32;
    let mut y1 = a[1].max(b[1]).round() as i32;
    if let Some([ox, oy, ow, oh]) = outer {
        x0 = x0.max(ox);
        y0 = y0.max(oy);
        x1 = x1.min(ox + ow);
        y1 = y1.min(oy + oh);
    }
    [x0, y0, (x1 - x0).max(0), (y1 - y0).max(0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Orthographic projection like the one XPLM sets up for 2D drawing.
    fn ortho(width: f32, height: f32) -> [f32; 16] {
        let mut m = [0.0; 16];
        m[0] = 2.0 / width;
        m[5] = 2.0 / height;
        m[10] = -1.0;
        m[12] = -1.0;
        m[13] = -1.0;
        m[15] = 1.0;
        m
    }

    /// Modelview for a scope at window (left, top): translate then flip y.
    fn scope_modelview(left: f32, top: f32) -> [f32; 16] {
        let mut m = [0.0; 16];
        m[0] = 1.0;
        m[5] = -1.0;
        m[10] = 1.0;
        m[12] = left;
        m[13] = top;
        m[15] = 1.0;
        m
    }

    fn assert_close(actual: [f32; 2], expected: [f32; 2]) {
        assert!(
            (actual[0] - expected[0]).abs() < 1e-3 && (actual[1] - expected[1]).abs() < 1e-3,
            "{actual:?} != {expected:?}"
        );
    }

    #[test]
    fn local_origin_maps_to_window_top_left() {
        // 1000x800 boxel screen rendered at 2x (2000x1600 pixels).
        let mvp = mat4_mul(&ortho(1000.0, 800.0), &scope_modelview(100.0, 700.0));
        let viewport = [0, 0, 2000, 1600];
        assert_close(to_pixels(&mvp, viewport, 0.0, 0.0), [200.0, 1400.0]);
        // 10 boxels right and 20 down from the window's top-left corner.
        assert_close(to_pixels(&mvp, viewport, 10.0, 20.0), [220.0, 1360.0]);
    }

    #[test]
    fn clip_rect_becomes_bottom_left_scissor_box() {
        let mvp = mat4_mul(&ortho(1000.0, 800.0), &scope_modelview(100.0, 700.0));
        let viewport = [0, 0, 1000, 800];
        let box_ = clip_to_scissor(&mvp, viewport, [0.0, 0.0, 50.0, 30.0], None);
        assert_eq!(box_, [100, 670, 50, 30]);
    }

    #[test]
    fn clip_is_intersected_with_outer_scissor() {
        let mvp = mat4_mul(&ortho(1000.0, 800.0), &scope_modelview(100.0, 700.0));
        let viewport = [0, 0, 1000, 800];
        let box_ = clip_to_scissor(
            &mvp,
            viewport,
            [0.0, 0.0, 50.0, 30.0],
            Some([120, 0, 500, 690]),
        );
        assert_eq!(box_, [120, 670, 30, 20]);
        let empty = clip_to_scissor(
            &mvp,
            viewport,
            [0.0, 0.0, 50.0, 30.0],
            Some([900, 0, 10, 10]),
        );
        assert_eq!(empty[2], 0);
    }
}
