//! A submenu under X-Plane's Plugins menu.

use std::ffi::{c_int, c_void};

use crate::guard;
use crate::sys;
use crate::util::to_cstring;

type Handler = Box<dyn FnMut(usize)>;

/// An owned submenu of the Plugins menu. Removed when dropped. Main thread only.
pub struct PluginsMenu {
    id: sys::XPLMMenuID,
    parent_index: c_int,
    next_item: usize,
    _handler: Box<Handler>,
}

impl PluginsMenu {
    /// Adds `name` to the Plugins menu with a submenu. `handler` receives the
    /// index returned by [`PluginsMenu::add_item`] when an item is chosen.
    pub fn new(name: &str, handler: impl FnMut(usize) + 'static) -> Self {
        let mut handler: Box<Handler> = Box::new(Box::new(handler));
        let name = to_cstring(name);
        unsafe {
            let plugins = sys::XPLMFindPluginsMenu();
            let parent_index =
                sys::XPLMAppendMenuItem(plugins, name.as_ptr(), std::ptr::null_mut(), 0);
            let id = sys::XPLMCreateMenu(
                name.as_ptr(),
                plugins,
                parent_index,
                Some(trampoline),
                handler.as_mut() as *mut Handler as *mut c_void,
            );
            Self {
                id,
                parent_index,
                next_item: 0,
                _handler: handler,
            }
        }
    }

    /// Appends an item; returns the index passed to the handler.
    pub fn add_item(&mut self, name: &str) -> usize {
        let index = self.next_item;
        self.next_item += 1;
        let name = to_cstring(name);
        unsafe { sys::XPLMAppendMenuItem(self.id, name.as_ptr(), index as *mut c_void, 0) };
        index
    }
}

impl Drop for PluginsMenu {
    fn drop(&mut self) {
        unsafe {
            sys::XPLMDestroyMenu(self.id);
            sys::XPLMRemoveMenuItem(sys::XPLMFindPluginsMenu(), self.parent_index);
        }
    }
}

unsafe extern "C" fn trampoline(menu_ref: *mut c_void, item_ref: *mut c_void) {
    // Menus stay usable after a failure so the user can reach the window.
    guard::guard_always("menu", (), || {
        let handler = unsafe { &mut *(menu_ref as *mut Handler) };
        handler(item_ref as usize);
    })
}
