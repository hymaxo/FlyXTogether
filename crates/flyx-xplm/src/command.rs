//! X-Plane commands: finding, creating, triggering, and intercepting them.

use std::ffi::{c_int, c_void};

use crate::guard;
use crate::sys;
use crate::util::to_cstring;

/// The phase a command handler is called in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The command was just pressed.
    Begin,
    /// The command is still held (called every frame).
    Continue,
    /// The command was released.
    End,
}

impl Phase {
    fn from_xplm(phase: sys::XPLMCommandPhase) -> Self {
        match phase as u32 {
            sys::xplm_CommandBegin => Phase::Begin,
            sys::xplm_CommandEnd => Phase::End,
            _ => Phase::Continue,
        }
    }
}

/// A command handle. Main thread only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    raw: sys::XPLMCommandRef,
}

impl Command {
    /// Looks up a command by name; `None` if no one has created it.
    pub fn find(name: &str) -> Option<Self> {
        let name = to_cstring(name);
        let raw = unsafe { sys::XPLMFindCommand(name.as_ptr()) };
        (!raw.is_null()).then_some(Self { raw })
    }

    /// Creates a command (or returns the existing one of that name), so users
    /// can bind it to keys and joystick buttons.
    pub fn create(name: &str, description: &str) -> Self {
        let name = to_cstring(name);
        let description = to_cstring(description);
        let raw = unsafe { sys::XPLMCreateCommand(name.as_ptr(), description.as_ptr()) };
        Self { raw }
    }

    /// Starts holding the command. Must be balanced by [`Command::end`].
    pub fn begin(&self) {
        unsafe { sys::XPLMCommandBegin(self.raw) }
    }

    /// Releases a command started with [`Command::begin`].
    pub fn end(&self) {
        unsafe { sys::XPLMCommandEnd(self.raw) }
    }

    /// Presses and releases the command.
    pub fn once(&self) {
        unsafe { sys::XPLMCommandOnce(self.raw) }
    }
}

type Handler = Box<dyn FnMut(Phase) -> bool>;

/// A registered command handler, unregistered when dropped. Main thread only.
///
/// The handler returns `true` to let X-Plane (and handlers after it) run the
/// command, `false` to swallow it. It may be called from inside
/// [`Command::begin`], [`Command::end`] or [`Command::once`] on the same
/// command, so it must not borrow state its caller already holds.
pub struct CommandHandler {
    command: Command,
    before: bool,
    handler: Box<Handler>,
}

impl CommandHandler {
    /// Registers `handler` on `command`, called before X-Plane's own handling
    /// if `before` is set, else after it.
    pub fn register(
        command: Command,
        before: bool,
        handler: impl FnMut(Phase) -> bool + 'static,
    ) -> Self {
        let mut handler: Box<Handler> = Box::new(Box::new(handler));
        unsafe {
            sys::XPLMRegisterCommandHandler(
                command.raw,
                Some(trampoline),
                before as c_int,
                handler.as_mut() as *mut Handler as *mut c_void,
            );
        }
        Self {
            command,
            before,
            handler,
        }
    }

    pub fn command(&self) -> Command {
        self.command
    }
}

impl Drop for CommandHandler {
    fn drop(&mut self) {
        unsafe {
            sys::XPLMUnregisterCommandHandler(
                self.command.raw,
                Some(trampoline),
                self.before as c_int,
                self.handler.as_mut() as *mut Handler as *mut c_void,
            );
        }
    }
}

unsafe extern "C" fn trampoline(
    _command: sys::XPLMCommandRef,
    phase: sys::XPLMCommandPhase,
    refcon: *mut c_void,
) -> c_int {
    // On a panic (or once the plugin has failed), let the command through so
    // the user's cockpit keeps working.
    guard::guard("command handler", 1, || {
        let handler = unsafe { &mut *(refcon as *mut Handler) };
        handler(Phase::from_xplm(phase)) as c_int
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_map_from_xplm() {
        assert_eq!(
            Phase::from_xplm(sys::xplm_CommandBegin as i32),
            Phase::Begin
        );
        assert_eq!(
            Phase::from_xplm(sys::xplm_CommandContinue as i32),
            Phase::Continue
        );
        assert_eq!(Phase::from_xplm(sys::xplm_CommandEnd as i32), Phase::End);
    }
}
