use super::*;

fn input(server: &str, login_id: &str, password: &str) -> LoginInput {
    LoginInput {
        server: server.into(),
        login_id: login_id.into(),
        password: password.into(),
        mfa: None,
    }
}

/// Catching an empty field here costs no round trip. It also costs no failed
/// login attempt, which matters: the login endpoint is rate-limited to 5/s
/// regardless of how the server is configured.
#[test]
fn an_incomplete_form_is_refused_before_it_reaches_the_network() {
    assert!(
        input("https://chat.example.com", "jane", "pw")
            .problem()
            .is_none()
    );

    assert!(input("", "jane", "pw").problem().is_some());
    assert!(
        input("https://chat.example.com", "", "pw")
            .problem()
            .is_some()
    );
    assert!(
        input("https://chat.example.com", "jane", "")
            .problem()
            .is_some()
    );
}

/// The server field is pre-filled with the scheme, which is a prompt rather
/// than an answer — submitting it unchanged must not be treated as a URL.
#[test]
fn the_prefilled_scheme_alone_does_not_count_as_a_server() {
    let problem = input("https://", "jane", "pw").problem();
    assert!(problem.is_some());
    assert!(problem.unwrap().contains("server"), "{problem:?}");
}

#[test]
fn each_missing_field_names_itself() {
    assert!(
        input("", "jane", "pw")
            .problem()
            .unwrap()
            .contains("server")
    );
    assert!(
        input("https://x", "", "pw")
            .problem()
            .unwrap()
            .contains("email")
    );
    assert!(
        input("https://x", "jane", "")
            .problem()
            .unwrap()
            .contains("password")
    );
}

/// A password manager that pastes a trailing space would otherwise fail with
/// "wrong password" for a reason nobody could see on screen.
#[test]
fn the_password_is_never_trimmed() {
    let padded = input("https://x", "jane", "  spaces matter  ");
    assert_eq!(padded.password, "  spaces matter  ");
    assert!(padded.problem().is_none());
}

#[test]
fn a_two_factor_code_is_optional() {
    let mut with_code = input("https://x", "jane", "pw");
    assert_eq!(with_code.mfa, None);
    with_code.mfa = Some("123456".into());
    assert!(with_code.problem().is_none());
    assert!(!format!("{with_code:?}").is_empty());
    assert!(!format!("{:?}", with_code.clone()).is_empty());
}
