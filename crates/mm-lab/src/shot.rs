//! A screenshot, for when a UI test fails.
//!
//! The widget tree says what the window *contained*; it cannot say what it
//! looked like. A test that fails on a runner nobody can log into needs
//! both, so a failure writes a PNG beside its message and CI keeps it as an
//! artefact.
//!
//! Through `x11rb` rather than `ImageMagick`'s `import`: it is pure Rust and
//! safe, so this needs no `unsafe` -- which the workspace forbids -- and no
//! tool installed on the runner beyond the X server the client is already
//! drawing into. On a display it cannot read it gives up and says so; a
//! missing screenshot must never turn a passing test red or hide why a
//! failing one failed.

use std::path::{Path, PathBuf};

use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};

use crate::{Error, Result};

/// Where on the screen to photograph: x, y, width, height.
pub type Area = (i32, i32, u16, u16);

/// Capture into `into`, named after `what`.
///
/// Read from the root window, because a popover is an X surface of its own
/// and a menu is what a failing test usually has open -- but cropped to
/// `area` when one is given. That crop is not tidiness: on a developer's
/// machine the root window is their whole desktop, and a test failure
/// should not quietly photograph whatever else they had open. On a runner
/// under Xvfb there is nothing else, and `None` captures the lot.
pub fn capture(into: &Path, what: &str, area: Option<Area>) -> Result<PathBuf> {
    let (conn, screen_number) =
        x11rb::connect(None).map_err(|e| Error(format!("no X display to photograph: {e}")))?;
    let screen = &conn.setup().roots[screen_number];
    let (x, y, width, height) = match area {
        Some((x, y, w, h)) => (
            x.max(0),
            y.max(0),
            w.min(
                screen
                    .width_in_pixels
                    .saturating_sub(u16::try_from(x.max(0)).unwrap_or(0)),
            ),
            h.min(
                screen
                    .height_in_pixels
                    .saturating_sub(u16::try_from(y.max(0)).unwrap_or(0)),
            ),
        ),
        None => (0, 0, screen.width_in_pixels, screen.height_in_pixels),
    };
    if width == 0 || height == 0 {
        return Err(Error("the area to photograph is empty".to_owned()));
    }

    let image = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            screen.root,
            i16::try_from(x).unwrap_or(0),
            i16::try_from(y).unwrap_or(0),
            width,
            height,
            u32::MAX,
        )
        .map_err(|e| Error(format!("asking X for the screen: {e}")))?
        .reply()
        .map_err(|e| Error(format!("reading the screen back: {e}")))?;

    // 32-bit Z_PIXMAP arrives as BGRX on every depth-24 display this will
    // ever run on. Written out as RGB rather than guessing at a mask: the
    // point is a picture somebody looks at, not colour fidelity.
    let mut rgb = Vec::with_capacity(usize::from(width) * usize::from(height) * 3);
    for pixel in image.data.chunks_exact(4) {
        rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
    }

    std::fs::create_dir_all(into)
        .map_err(|e| Error(format!("cannot create {}: {e}", into.display())))?;
    let path = into.join(format!("{}.png", slug(what)));
    let file = std::fs::File::create(&path)
        .map_err(|e| Error(format!("cannot write {}: {e}", path.display())))?;
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(file),
        u32::from(width),
        u32::from(height),
    );
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut w| w.write_image_data(&rgb))
        .map_err(|e| Error(format!("cannot encode {}: {e}", path.display())))?;
    Ok(path)
}

/// A scenario's name as a filename: no spaces, nothing a shell would eat.
fn slug(what: &str) -> String {
    what.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_lowercase()
}
