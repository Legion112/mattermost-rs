//! The sign-in screen.
//!
//! The password is typed here, held in a `String` only as long as it takes to
//! build the login request, and **never written to disk** — only the session
//! token the server returns is persisted. It is not logged, not put in an
//! argument, and not placed in the environment.
//!
//! The two-factor row is hidden until the server asks for a code. Showing it
//! unconditionally would make every sign-in look like it needs one.

// adw::prelude re-exports gtk::prelude, so this one import covers both.
use adw::prelude::*;

/// The widgets the window needs to drive.
pub(crate) struct LoginPage {
    pub(crate) root: gtk::Widget,
    pub(crate) server: adw::EntryRow,
    pub(crate) login_id: adw::EntryRow,
    pub(crate) password: adw::PasswordEntryRow,
    pub(crate) mfa: adw::EntryRow,
    pub(crate) submit: gtk::Button,
    pub(crate) message: gtk::Label,
}

impl std::fmt::Debug for LoginPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LoginPage")
    }
}

impl LoginPage {
    pub(crate) fn new() -> Self {
        let server = adw::EntryRow::builder().title("Server").build();
        server.set_text("https://");
        let login_id = adw::EntryRow::builder().title("Email or username").build();
        let password = adw::PasswordEntryRow::builder().title("Password").build();

        let mfa = adw::EntryRow::builder().title("Two-factor code").build();
        // Hidden until the server says it wants one.
        mfa.set_visible(false);

        let group = adw::PreferencesGroup::builder().build();
        group.add(&server);
        group.add(&login_id);
        group.add(&password);
        group.add(&mfa);

        let submit = gtk::Button::builder()
            .label("Log in")
            .css_classes(["suggested-action", "pill"])
            .halign(gtk::Align::Center)
            .build();

        let message = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["dim-label"])
            .build();

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .width_request(420)
            .margin_top(24)
            .margin_bottom(24)
            .build();
        column.append(&group);
        column.append(&submit);
        column.append(&message);

        let page = adw::StatusPage::builder()
            .icon_name("user-available-symbolic")
            .title("mattermost-rs")
            .description("Sign in to your Mattermost server")
            .child(&column)
            .build();

        let scroller = gtk::ScrolledWindow::builder()
            .child(&page)
            .vexpand(true)
            .build();

        Self {
            root: scroller.upcast(),
            server,
            login_id,
            password,
            mfa,
            submit,
            message,
        }
    }

    /// Everything typed in, trimmed, with the password left exactly as typed.
    ///
    /// Trimming the password would silently change it — leading or trailing
    /// spaces are legal characters, and a password manager that pastes one
    /// would then fail with "wrong password" for a reason nobody could see.
    pub(crate) fn values(&self) -> LoginInput {
        let mfa = self.mfa.text().trim().to_owned();
        LoginInput {
            server: self.server.text().trim().to_owned(),
            login_id: self.login_id.text().trim().to_owned(),
            password: self.password.text().to_string(),
            mfa: (!mfa.is_empty()).then_some(mfa),
        }
    }

    /// Forget the password as soon as the request has been built.
    ///
    /// This is hygiene, not a security guarantee: GTK owns the buffer and Rust
    /// strings are not zeroed on drop, so copies may well survive in memory.
    /// It does mean the password is not sitting in a visible widget for the
    /// rest of the session.
    pub(crate) fn clear_password(&self) {
        self.password.set_text("");
    }

    pub(crate) fn set_busy(&self, busy: bool) {
        self.submit.set_sensitive(!busy);
        self.server.set_sensitive(!busy);
        self.login_id.set_sensitive(!busy);
        self.password.set_sensitive(!busy);
        self.mfa.set_sensitive(!busy);
        self.submit
            .set_label(if busy { "Signing in…" } else { "Log in" });
    }

    pub(crate) fn show_error(&self, text: &str) {
        self.message.set_text(text);
        self.message.remove_css_class("dim-label");
        self.message.add_css_class("error");
    }

    pub(crate) fn show_hint(&self, text: &str) {
        self.message.set_text(text);
        self.message.remove_css_class("error");
        self.message.add_css_class("dim-label");
    }

    /// Reveal the two-factor row and put the cursor in it.
    pub(crate) fn ask_for_mfa(&self) {
        self.mfa.set_visible(true);
        self.mfa.grab_focus();
    }
}

/// What the form holds.
#[derive(Debug, Clone)]
pub(crate) struct LoginInput {
    pub(crate) server: String,
    pub(crate) login_id: String,
    pub(crate) password: String,
    pub(crate) mfa: Option<String>,
}

impl LoginInput {
    /// Why this cannot be submitted yet, if it cannot.
    ///
    /// Checked here rather than left to the server so that an obvious typo
    /// costs no round trip and no failed-login attempt — the login endpoint is
    /// rate-limited to 5/s regardless of server configuration.
    pub(crate) fn problem(&self) -> Option<&'static str> {
        if self.server.is_empty() || self.server == "https://" {
            return Some("Enter the address of your Mattermost server.");
        }
        if self.login_id.is_empty() {
            return Some("Enter your email address or username.");
        }
        if self.password.is_empty() {
            return Some("Enter your password.");
        }
        None
    }
}

#[cfg(test)]
#[path = "tests/login.rs"]
mod tests;
