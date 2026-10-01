//! The FlyXTogether window: connection status, host and join forms.
//!
//! The window renders the session state and records what the user asked
//! for as [`UiAction`]s; the plugin drains those each frame.

use flyx_sync::Password;
use flyx_sync::session::{Notice, State};
use flyx_xplm::imgui::{Condition, StyleColor, Ui, WindowFlags};

/// Initial window size in boxels.
pub const WINDOW_SIZE: (i32, i32) = (440, 380);

const GREY: [f32; 4] = [0.70, 0.70, 0.70, 1.0];
const AMBER: [f32; 4] = [1.00, 0.78, 0.25, 1.0];
const BLUE: [f32; 4] = [0.45, 0.70, 1.00, 1.0];
const GREEN: [f32; 4] = [0.40, 0.90, 0.45, 1.0];
const RED: [f32; 4] = [1.00, 0.40, 0.35, 1.0];

/// Shown while an aircraft no profile verifies is loaded.
pub const UNTESTED_AIRCRAFT: &str = "Untested aircraft: some systems may not sync.";

/// What the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    Host {
        port: u16,
        password: Password,
        name: String,
    },
    Join {
        address: String,
        password: Password,
        name: String,
    },
    /// Leave the session, cancel a join, or stop hosting.
    Leave,
    /// Take the flight controls.
    TakeControls,
}

/// Editable form fields. The passwords live only in memory.
#[derive(Debug, Default)]
pub struct Form {
    pub name: String,
    pub host_port: String,
    pub host_password: String,
    pub join_address: String,
    pub join_password: String,
}

#[derive(Debug)]
pub struct UiModel {
    pub state: State,
    pub notice: Option<Notice>,
    /// Set when the plugin has stopped after an internal error.
    pub failure: Option<String>,
    pub log_path: String,
    pub form: Form,
    /// "You have the controls" or "<name> has the controls", while connected.
    pub controls_line: Option<String>,
    pub can_take_controls: bool,
    /// The loaded aircraft is not verified by a profile.
    pub untested_aircraft: bool,
    actions: Vec<UiAction>,
    #[cfg(feature = "dev")]
    pub show_demo: bool,
    /// Shown as the failure when the plugin has not really failed.
    #[cfg(feature = "dev")]
    pub failure_preview: Option<String>,
}

impl UiModel {
    pub fn new(log_path: String) -> Self {
        Self {
            state: State::Idle,
            notice: None,
            failure: None,
            log_path,
            form: Form::default(),
            controls_line: None,
            can_take_controls: false,
            untested_aircraft: false,
            actions: Vec::new(),
            #[cfg(feature = "dev")]
            show_demo: false,
            #[cfg(feature = "dev")]
            failure_preview: None,
        }
    }

    pub fn take_actions(&mut self) -> Vec<UiAction> {
        std::mem::take(&mut self.actions)
    }

    fn error(&mut self, text: &str) {
        self.notice = Some(Notice::Error(text.to_owned()));
    }

    /// Builds the window content. Called once per frame from the draw callback.
    pub fn draw(&mut self, ui: &Ui) {
        let size = ui.io().display_size;
        ui.window("##flyxtogether")
            .position([0.0, 0.0], Condition::Always)
            .size(size, Condition::Always)
            .flags(
                WindowFlags::NO_TITLE_BAR
                    | WindowFlags::NO_RESIZE
                    | WindowFlags::NO_MOVE
                    | WindowFlags::NO_COLLAPSE
                    | WindowFlags::NO_SAVED_SETTINGS,
            )
            .build(|| self.draw_content(ui));

        #[cfg(feature = "dev")]
        if self.show_demo {
            ui.show_demo_window(&mut self.show_demo);
        }
    }

    fn draw_content(&mut self, ui: &Ui) {
        #[cfg(feature = "dev")]
        if self.failure.is_none() {
            self.failure = self.failure_preview.clone();
        }
        if let Some(failure) = &self.failure {
            ui.text_colored(RED, "FlyXTogether stopped after an internal error");
            ui.text_wrapped(failure);
            ui.spacing();
            ui.text_wrapped(format!(
                "Restart X-Plane to use FlyXTogether again. Details are in {}",
                self.log_path
            ));
            return;
        }

        self.draw_status(ui);
        if self.untested_aircraft {
            ui.text_colored(AMBER, UNTESTED_AIRCRAFT);
        }
        if let Some(notice) = &self.notice {
            match notice {
                Notice::Info(text) => ui.text_wrapped(text),
                Notice::Error(text) => {
                    let _c = ui.push_style_color(StyleColor::Text, RED);
                    ui.text_wrapped(text);
                }
            }
        }
        ui.separator();

        let idle = self.state == State::Idle;
        ui.set_next_item_width(200.0);
        ui.disabled(!idle, || {
            ui.input_text("Your name", &mut self.form.name).build();
        });
        ui.spacing();

        match self.state.clone() {
            State::Idle => self.draw_idle_forms(ui),
            State::StartingHost { .. } | State::Joining { .. } => {
                if ui.button("Cancel") {
                    self.actions.push(UiAction::Leave);
                }
            }
            State::Hosting {
                addresses, crew, ..
            } => {
                if crew.is_none() {
                    ui.text_wrapped(
                        "Give your crew your public IP address and this port. \
                         The port must be forwarded to this computer.",
                    );
                    if !addresses.is_empty() {
                        ui.text_disabled("Addresses of this computer:");
                        for a in &addresses {
                            ui.bullet_text(a);
                        }
                    }
                    ui.spacing();
                }
                if crew.is_some() {
                    self.draw_take_controls(ui);
                }
                if ui.button("Stop hosting") {
                    self.actions.push(UiAction::Leave);
                }
            }
            State::Joined { .. } => {
                self.draw_take_controls(ui);
                if ui.button("Leave session") {
                    self.actions.push(UiAction::Leave);
                }
            }
        }

        #[cfg(feature = "dev")]
        {
            ui.separator();
            ui.checkbox("Show ImGui demo (dev)", &mut self.show_demo);
        }
    }

    fn draw_take_controls(&mut self, ui: &Ui) {
        let can = self.can_take_controls;
        ui.disabled(!can, || {
            if ui.button("Take controls") {
                self.actions.push(UiAction::TakeControls);
            }
        });
        ui.same_line();
    }

    fn draw_status(&self, ui: &Ui) {
        let controls = || {
            if let Some(line) = &self.controls_line {
                ui.text_colored(BLUE, line);
            }
        };
        match &self.state {
            State::Idle => ui.text_colored(GREY, "Not connected"),
            State::StartingHost { port } => {
                ui.text_colored(BLUE, format!("Starting to host on port {port}..."));
            }
            State::Hosting {
                port, crew: None, ..
            } => {
                ui.text_colored(AMBER, "Hosting - waiting for crew");
                ui.text(format!("Port {port} (UDP)"));
            }
            State::Hosting {
                port,
                crew: Some(name),
                ..
            } => {
                ui.text_colored(GREEN, "Connected");
                ui.text(format!("Crew: {name}"));
                controls();
                ui.text_disabled(format!("Hosting on port {port} (UDP)"));
            }
            State::Joining { address } => {
                ui.text_colored(BLUE, format!("Connecting to {address}..."));
            }
            State::Joined { host, address } => {
                ui.text_colored(GREEN, "Connected");
                ui.text(format!("Host: {host}"));
                controls();
                ui.text_disabled(format!("Host address: {address}"));
            }
        }
    }

    fn draw_idle_forms(&mut self, ui: &Ui) {
        let Some(_bar) = ui.tab_bar("mode") else {
            return;
        };
        if let Some(_tab) = ui.tab_item("Host") {
            ui.set_next_item_width(100.0);
            ui.input_text("Port (UDP)", &mut self.form.host_port)
                .chars_decimal(true)
                .build();
            ui.set_next_item_width(200.0);
            ui.input_text("Session password##host", &mut self.form.host_password)
                .password(true)
                .build();
            if ui.button("Host") {
                self.submit_host();
            }
        }
        if let Some(_tab) = ui.tab_item("Join") {
            ui.set_next_item_width(260.0);
            ui.input_text("Host address", &mut self.form.join_address)
                .hint("203.0.113.7:49700")
                .chars_noblank(true)
                .build();
            ui.set_next_item_width(200.0);
            ui.input_text("Session password##join", &mut self.form.join_password)
                .password(true)
                .build();
            if ui.button("Join") {
                self.submit_join();
            }
        }
    }

    fn submit_host(&mut self) {
        let port = match self.form.host_port.trim().parse::<u16>() {
            Ok(p) if p > 0 => p,
            _ => return self.error("Enter a port number between 1 and 65535."),
        };
        if self.form.host_password.is_empty() {
            return self.error("Enter a session password to host.");
        }
        self.notice = None;
        self.actions.push(UiAction::Host {
            port,
            password: Password::new(self.form.host_password.clone()),
            name: self.display_name(),
        });
    }

    fn submit_join(&mut self) {
        let address = self.form.join_address.trim().to_owned();
        if address.is_empty() {
            return self.error("Enter the host's address, for example 203.0.113.7:49700.");
        }
        if self.form.join_password.is_empty() {
            return self.error("Enter the session password.");
        }
        self.notice = None;
        self.actions.push(UiAction::Join {
            address,
            password: Password::new(self.form.join_password.clone()),
            name: self.display_name(),
        });
    }

    fn display_name(&self) -> String {
        let name = self.form.name.trim();
        if name.is_empty() {
            "Pilot".to_owned()
        } else {
            name.to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> UiModel {
        let mut m = UiModel::new("FlyXTogether.log".into());
        m.form.host_port = "49700".into();
        m
    }

    /// The form messages are listed word for word in docs/hosting.md.
    #[test]
    fn hosting_doc_lists_form_messages() {
        let doc = include_str!("../../../docs/hosting.md");
        let mut m = model();
        let mut texts = Vec::new();
        m.form.host_port = "0".into();
        m.submit_host();
        texts.push(m.notice.take());
        m.form.host_port = "49700".into();
        m.submit_host();
        texts.push(m.notice.take());
        m.submit_join();
        texts.push(m.notice.take());
        m.form.join_address = "203.0.113.7".into();
        m.submit_join();
        texts.push(m.notice.take());
        for notice in texts {
            let Some(Notice::Error(text)) = notice else {
                panic!("expected an error notice");
            };
            assert!(doc.contains(&text), "docs/hosting.md is missing: {text}");
        }
    }

    /// The untested-aircraft line is quoted in the hosting doc.
    #[test]
    fn hosting_doc_mentions_untested_aircraft() {
        let doc = include_str!("../../../docs/hosting.md");
        assert!(doc.contains(UNTESTED_AIRCRAFT));
    }

    #[test]
    fn host_requires_password() {
        let mut m = model();
        m.submit_host();
        assert!(m.take_actions().is_empty());
        assert_eq!(
            m.notice,
            Some(Notice::Error("Enter a session password to host.".into()))
        );
    }

    #[test]
    fn host_rejects_bad_port() {
        let mut m = model();
        m.form.host_password = "secret".into();
        for bad in ["", "0", "70000", "abc"] {
            m.form.host_port = bad.into();
            m.submit_host();
            assert!(m.take_actions().is_empty(), "port {bad:?} accepted");
        }
    }

    #[test]
    fn host_submits_action_with_default_name() {
        let mut m = model();
        m.form.host_password = "secret".into();
        m.submit_host();
        assert_eq!(
            m.take_actions(),
            vec![UiAction::Host {
                port: 49700,
                password: Password::new("secret"),
                name: "Pilot".into()
            }]
        );
        assert_eq!(m.notice, None);
    }

    #[test]
    fn join_requires_address_and_password() {
        let mut m = model();
        m.submit_join();
        assert!(matches!(m.notice, Some(Notice::Error(_))));
        m.form.join_address = " 203.0.113.7:49700 ".into();
        m.submit_join();
        assert_eq!(
            m.notice,
            Some(Notice::Error("Enter the session password.".into()))
        );
        m.form.join_password = "secret".into();
        m.form.name = "Alex".into();
        m.submit_join();
        assert_eq!(
            m.take_actions(),
            vec![UiAction::Join {
                address: "203.0.113.7:49700".into(),
                password: Password::new("secret"),
                name: "Alex".into()
            }]
        );
    }
}
