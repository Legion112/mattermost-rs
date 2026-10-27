use super::*;

/// A control file is parsed by `dpkg` as RFC822-ish fields: one
/// `Key: value` per line, and a continuation line in the description
/// must begin with a space or the field ends early and the rest of the
/// file is read as garbage.
#[test]
fn the_control_file_is_shaped_the_way_dpkg_reads_it() {
    let control = render_control("0.1.0", "amd64", 9_000);

    assert!(control.starts_with("Package: mattermost-rs\n"));
    assert!(control.contains("Version: 0.1.0\n"));
    assert!(control.contains("Architecture: amd64\n"));
    assert!(control.contains("Installed-Size: 9000\n"));
    assert!(control.ends_with('\n'));

    // Every line is either a field or an indented continuation.
    for line in control.lines() {
        let ok = line.starts_with(' ') || line.split_once(": ").is_some();
        assert!(ok, "not a field or continuation: {line:?}");
    }
    // The description's body is continuation lines, which is the part
    // that is wrong in half the hand-written control files anywhere.
    let body: Vec<&str> = control
        .lines()
        .skip_while(|l| !l.starts_with("Description:"))
        .skip(1)
        .collect();
    assert!(!body.is_empty(), "the description needs a body");
    assert!(body.iter().all(|l| l.starts_with(' ')), "{body:?}");
}

/// What it needs, and what merely makes it good. Getting the second
/// list into the first would mean refusing to install on a machine
/// where the client would work perfectly well without spelling or a
/// tray.
#[test]
fn dictionaries_and_the_tray_are_recommended_not_required() {
    let control = render_control("0.1.0", "amd64", 1);
    let depends = control
        .lines()
        .find_map(|l| l.strip_prefix("Depends: "))
        .expect("a Depends field");
    let recommends = control
        .lines()
        .find_map(|l| l.strip_prefix("Recommends: "))
        .expect("a Recommends field");

    assert!(depends.contains("libgtk-4-1"), "{depends}");
    assert!(depends.contains("libadwaita-1-0"), "{depends}");
    // Without GTK it cannot start; without a dictionary it just draws
    // no underlines.
    assert!(!depends.contains("hunspell"), "{depends}");
    assert!(recommends.contains("hunspell-en-us"), "{recommends}");
    assert!(recommends.contains("appindicator"), "{recommends}");

    // Comma-space separated, which is what dpkg expects.
    for field in [depends, recommends] {
        assert!(!field.contains(",,"), "{field}");
        assert!(!field.ends_with(','), "{field}");
    }
}
