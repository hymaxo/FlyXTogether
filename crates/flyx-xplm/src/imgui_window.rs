//! Dear ImGui inside an XPLM window: input translation and rendering.

use std::time::Instant;

use imgui::{ClipboardBackend, Context, DrawCmd, DrawCmdParams, DrawData, Key, TextureId, Ui};

use crate::gl::{Scope2d, Texture};
use crate::sys;
use crate::window::{
    Geometry, KeyEvent, MouseButton, MouseStatus, Window, WindowContext, WindowDelegate,
};

/// Position ImGui treats as "no mouse".
const NO_MOUSE: [f32; 2] = [-f32::MAX, -f32::MAX];
/// Position far outside the window, used to click "into the void".
const VOID: [f32; 2] = [-10_000.0, -10_000.0];

/// An XPLM window whose content is drawn by an ImGui build closure.
pub struct ImguiWindow {
    window: Window,
}

impl ImguiWindow {
    /// Creates a hidden window. Only one ImGui window may exist at a time.
    pub fn new(title: &str, width: i32, height: i32, build: impl FnMut(&Ui) + 'static) -> Self {
        let delegate = ImguiDelegate::new(Box::new(build));
        Self {
            window: Window::new(title, width, height, false, delegate),
        }
    }

    pub fn window(&self) -> &Window {
        &self.window
    }
}

struct ImguiDelegate {
    imgui: Context,
    /// Created on the first draw, where a GL context is guaranteed.
    font_texture: Option<Texture>,
    last_frame: Instant,
    build: Box<dyn FnMut(&Ui)>,
    /// Latest cursor position reported by X-Plane (used in VR).
    reported_cursor: Option<[f32; 2]>,
    buttons_down: u8,
    had_text_input: bool,
    focus_taken_away: bool,
}

/// The system clipboard, so text copied in other programs can be pasted
/// (Ctrl+V) and text copied here can be pasted elsewhere. Opened on first
/// use; without one, ImGui only copies and pastes within itself.
#[derive(Default)]
struct OsClipboard {
    clipboard: Option<arboard::Clipboard>,
}

impl OsClipboard {
    fn open(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(c) => self.clipboard = Some(c),
                Err(e) => tracing::warn!(%e, "system clipboard unavailable"),
            }
        }
        self.clipboard.as_mut()
    }
}

impl ClipboardBackend for OsClipboard {
    fn get(&mut self) -> Option<String> {
        self.open()?.get_text().ok()
    }

    fn set(&mut self, value: &str) {
        if let Some(c) = self.open() {
            let _ = c.set_text(value.to_owned());
        }
    }
}

impl ImguiDelegate {
    fn new(build: Box<dyn FnMut(&Ui)>) -> Self {
        let mut imgui = Context::create();
        imgui.set_ini_filename(None);
        imgui.set_log_filename(None);
        imgui.set_clipboard_backend(OsClipboard::default());
        let style = imgui.style_mut();
        style.window_rounding = 0.0;
        style.frame_rounding = 3.0;
        Self {
            imgui,
            font_texture: None,
            last_frame: Instant::now(),
            build,
            reported_cursor: None,
            buttons_down: 0,
            had_text_input: false,
            focus_taken_away: false,
        }
    }

    fn ensure_font_texture(&mut self) {
        if self.font_texture.is_some() {
            return;
        }
        let fonts = self.imgui.fonts();
        let atlas = fonts.build_rgba32_texture();
        let texture = Texture::from_rgba(atlas.width, atlas.height, atlas.data);
        fonts.tex_id = TextureId::new(texture.id() as usize);
        self.font_texture = Some(texture);
    }
}

fn local(g: &Geometry, x: i32, y: i32) -> [f32; 2] {
    [(x - g.left) as f32, (g.top - y) as f32]
}

impl WindowDelegate for ImguiDelegate {
    fn draw(&mut self, ctx: &WindowContext) {
        let g = ctx.geometry();
        if g.width() <= 0 || g.height() <= 0 {
            return;
        }
        self.ensure_font_texture();

        let now = Instant::now();
        let dt = now
            .duration_since(self.last_frame)
            .as_secs_f32()
            .max(1.0e-4);
        self.last_frame = now;

        let mouse = match ctx.mouse_location() {
            Some((x, y)) if g.contains(x, y) || self.buttons_down > 0 => Some(local(&g, x, y)),
            Some(_) => None,
            None => self.reported_cursor.take(),
        };

        let io = self.imgui.io_mut();
        io.display_size = [g.width() as f32, g.height() as f32];
        io.delta_time = dt;
        if self.focus_taken_away {
            // Keyboard focus went elsewhere: click into the void so ImGui
            // deactivates the text field instead of us taking focus back.
            self.focus_taken_away = false;
            io.add_mouse_pos_event(VOID);
            io.add_mouse_button_event(imgui::MouseButton::Left, true);
            io.add_mouse_button_event(imgui::MouseButton::Left, false);
        }
        io.add_mouse_pos_event(mouse.unwrap_or(NO_MOUSE));

        let ui = self.imgui.new_frame();
        (self.build)(ui);
        let draw_data = self.imgui.render();
        render(draw_data, &g);

        // Take keyboard focus when a text field becomes active, give it back
        // when it stops being active.
        let want_text = self.imgui.io().want_text_input;
        if want_text != self.had_text_input {
            ctx.request_keyboard_focus(want_text);
            self.had_text_input = want_text;
        }
    }

    fn mouse(
        &mut self,
        ctx: &WindowContext,
        x: i32,
        y: i32,
        button: MouseButton,
        status: MouseStatus,
    ) -> bool {
        let g = ctx.geometry();
        let io = self.imgui.io_mut();
        io.add_mouse_pos_event(local(&g, x, y));
        let button = match button {
            MouseButton::Left => imgui::MouseButton::Left,
            MouseButton::Right => imgui::MouseButton::Right,
        };
        match status {
            MouseStatus::Down => {
                self.buttons_down = self.buttons_down.saturating_add(1);
                io.add_mouse_button_event(button, true);
            }
            MouseStatus::Up => {
                self.buttons_down = self.buttons_down.saturating_sub(1);
                io.add_mouse_button_event(button, false);
            }
            MouseStatus::Drag => {}
        }
        true
    }

    fn cursor(&mut self, ctx: &WindowContext, x: i32, y: i32) {
        self.reported_cursor = Some(local(&ctx.geometry(), x, y));
    }

    fn wheel(&mut self, _ctx: &WindowContext, _x: i32, _y: i32, axis: i32, clicks: i32) -> bool {
        let amount = clicks as f32;
        let wheel = if axis == 0 {
            [0.0, amount]
        } else {
            [amount, 0.0]
        };
        self.imgui.io_mut().add_mouse_wheel_event(wheel);
        true
    }

    fn key(&mut self, _ctx: &WindowContext, event: KeyEvent) {
        // X-Plane repeats key-down while a key is held and its key-up events
        // are not reliable, so each key-down becomes a full press.
        if !event.down {
            return;
        }
        let io = self.imgui.io_mut();
        io.add_key_event(Key::ModCtrl, event.control);
        io.add_key_event(Key::ModShift, event.shift);
        io.add_key_event(Key::ModAlt, event.alt);
        // On macOS, X-Plane reports Command as the control flag, and ImGui
        // expects Command (Super) for copy and paste.
        #[cfg(target_os = "macos")]
        io.add_key_event(Key::ModSuper, event.control);
        if let Some(key) = map_key(event.virtual_key) {
            io.add_key_event(key, true);
            io.add_key_event(key, false);
        }
        if let Some(ch) = event.ch
            && !event.control
        {
            io.add_input_character(ch);
        }
        io.add_key_event(Key::ModCtrl, false);
        io.add_key_event(Key::ModShift, false);
        io.add_key_event(Key::ModAlt, false);
        #[cfg(target_os = "macos")]
        io.add_key_event(Key::ModSuper, false);
    }

    fn focus_lost(&mut self, _ctx: &WindowContext) {
        if self.had_text_input {
            self.focus_taken_away = true;
        }
    }
}

fn map_key(virtual_key: u8) -> Option<Key> {
    let vk = virtual_key as i32;
    Some(match vk {
        sys::XPLM_VK_BACK => Key::Backspace,
        sys::XPLM_VK_TAB => Key::Tab,
        sys::XPLM_VK_RETURN => Key::Enter,
        sys::XPLM_VK_ENTER | sys::XPLM_VK_NUMPAD_ENT => Key::KeypadEnter,
        sys::XPLM_VK_ESCAPE => Key::Escape,
        sys::XPLM_VK_SPACE => Key::Space,
        sys::XPLM_VK_PRIOR => Key::PageUp,
        sys::XPLM_VK_NEXT => Key::PageDown,
        sys::XPLM_VK_END => Key::End,
        sys::XPLM_VK_HOME => Key::Home,
        sys::XPLM_VK_LEFT => Key::LeftArrow,
        sys::XPLM_VK_UP => Key::UpArrow,
        sys::XPLM_VK_RIGHT => Key::RightArrow,
        sys::XPLM_VK_DOWN => Key::DownArrow,
        sys::XPLM_VK_INSERT => Key::Insert,
        sys::XPLM_VK_DELETE => Key::Delete,
        sys::XPLM_VK_A => Key::A,
        sys::XPLM_VK_C => Key::C,
        sys::XPLM_VK_V => Key::V,
        sys::XPLM_VK_X => Key::X,
        sys::XPLM_VK_Y => Key::Y,
        sys::XPLM_VK_Z => Key::Z,
        _ => return None,
    })
}

fn render(draw_data: &DrawData, g: &Geometry) {
    let scope = Scope2d::begin(g.left as f32, g.top as f32);
    let [ox, oy] = draw_data.display_pos;
    for list in draw_data.draw_lists() {
        let vertices = list.vtx_buffer();
        let indices = list.idx_buffer();
        for command in list.commands() {
            if let DrawCmd::Elements {
                count,
                cmd_params:
                    DrawCmdParams {
                        clip_rect,
                        texture_id,
                        idx_offset,
                        ..
                    },
            } = command
            {
                let clip = [
                    clip_rect[0] - ox,
                    clip_rect[1] - oy,
                    clip_rect[2] - ox,
                    clip_rect[3] - oy,
                ];
                if clip[2] <= clip[0] || clip[3] <= clip[1] {
                    continue;
                }
                scope.set_clip(clip);
                let Some(slice) = indices.get(idx_offset..idx_offset + count) else {
                    continue;
                };
                scope.draw(texture_id.id() as i32, vertices, slice);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_editing_keys() {
        assert_eq!(map_key(sys::XPLM_VK_BACK as u8), Some(Key::Backspace));
        assert_eq!(map_key(sys::XPLM_VK_ENTER as u8), Some(Key::KeypadEnter));
        assert_eq!(map_key(sys::XPLM_VK_V as u8), Some(Key::V));
        assert_eq!(map_key(0x70), None);
    }

    #[test]
    fn local_coordinates_flip_y() {
        let g = Geometry {
            left: 100,
            top: 700,
            right: 500,
            bottom: 400,
        };
        assert_eq!(local(&g, 100, 700), [0.0, 0.0]);
        assert_eq!(local(&g, 150, 650), [50.0, 50.0]);
    }
}
