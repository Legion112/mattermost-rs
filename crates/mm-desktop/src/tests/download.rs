use super::*;

/// A file name comes off the wire and whoever sent the message chose
/// it. `../../../.bashrc` is a file name, and saving under it would let
/// anybody in a channel write anywhere the client can reach.
#[test]
fn a_name_from_the_wire_cannot_point_outside_the_folder() {
    // The separators are what made it dangerous; the leading dot is
    // just a hidden file, and renaming it would save a different file.
    assert_eq!(safe_name("../../../.bashrc"), ".bashrc");
    assert_eq!(safe_name("/etc/passwd"), "passwd");
    assert_eq!(safe_name(r"..\..\windows\system32"), "system32");
    assert_eq!(safe_name("report.pdf"), "report.pdf");
    // Nothing left to save under is still saved, somewhere obvious.
    assert_eq!(safe_name(".."), "attachment");
    assert_eq!(safe_name("/"), "attachment");
    assert_eq!(safe_name("   "), "attachment");
    assert_eq!(safe_name(""), "attachment");
    // Control characters would make a name that lies about itself in
    // any listing that prints it.
    assert_eq!(safe_name("ok\u{7}\u{0}.png"), "ok.png");
}

/// Saving the same screenshot twice keeps both, the way every browser
/// does it.
#[test]
fn a_name_already_taken_grows_a_counter() {
    let folder = Path::new("/home/legion/Downloads");
    let free = |_: &Path| false;
    assert_eq!(
        destination(folder, "shot.png", free),
        folder.join("shot.png")
    );

    let held: Vec<PathBuf> = ["shot.png", "shot (1).png"]
        .iter()
        .map(|n| folder.join(n))
        .collect();
    let taken = |p: &Path| held.iter().any(|h| h == p);
    assert_eq!(
        destination(folder, "shot.png", taken),
        folder.join("shot (2).png")
    );

    // No extension, and a hidden file whose leading dot is its name
    // rather than an extension.
    let one = |p: &Path| p == folder.join("README") || p == folder.join(".bashrc");
    assert_eq!(
        destination(folder, "README", one),
        folder.join("README (1)")
    );
    assert_eq!(
        destination(folder, ".bashrc", one),
        folder.join(".bashrc (1)")
    );

    // A folder somebody filled with every variant must not spin: the
    // last one is reused, which beats hanging.
    assert_eq!(
        destination(folder, "shot.png", |_| true),
        folder.join("shot (999).png")
    );
    // And the sanitising applies on the way in.
    assert_eq!(
        destination(folder, "../shot.png", free),
        folder.join("shot.png")
    );
}
