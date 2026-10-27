use super::*;

/// A size worth restoring is kept; anything that could only have come
/// from a window being measured mid-unmap, or from a monitor that is no
/// longer attached, falls back to the default rather than opening the
/// client as a postage stamp or off the side of the screen.
#[test]
fn only_a_usable_size_is_restored() {
    let half_a_screen = Geometry {
        width: 1_280,
        height: 1_440,
        maximized: false,
    };
    assert!(half_a_screen.worth_saving());
    assert_eq!(half_a_screen.sane(), half_a_screen);

    let default = Geometry::default();
    for bad in [
        Geometry {
            width: 0,
            ..half_a_screen
        },
        Geometry {
            width: 12,
            ..half_a_screen
        },
        Geometry {
            height: -1,
            ..half_a_screen
        },
        Geometry {
            width: 99_999,
            ..half_a_screen
        },
    ] {
        assert!(!bad.worth_saving(), "{bad:?}");
        let fixed = bad.sane();
        assert!(fixed.width >= 480 && fixed.height >= 360, "{fixed:?}");
        assert!(fixed.width <= 16_384 && fixed.height <= 16_384, "{fixed:?}");
    }
    assert_eq!(
        Geometry {
            width: 0,
            height: 0,
            maximized: false,
        }
        .sane(),
        default
    );

    // Maximized travels through untouched: the size beside it is what
    // to go back to when it is un-maximized again.
    let big = Geometry {
        maximized: true,
        ..half_a_screen
    };
    assert!(big.sane().maximized);
    assert!(big.worth_saving());

    // Every derive is exercised.
    assert_eq!(big, big);
    assert!(!format!("{big:?}").is_empty());
    let json = serde_json::to_string(&big).unwrap();
    assert_eq!(serde_json::from_str::<Geometry>(&json).unwrap(), big);
}

/// The window comes back where the reader left it: the same size, and
/// the same channel. A channel id that no longer exists must read as
/// "no channel" rather than fail the whole file, and an old file with
/// no channel in it at all still restores the size.
#[test]
fn the_open_channel_travels_with_the_size() {
    let left = Restored {
        geometry: Geometry {
            width: 1_280,
            height: 1_400,
            maximized: false,
        },
        channel: "c1".to_owned(),
    };
    let json = serde_json::to_string(&left).unwrap();
    // Flattened, so the file reads as one object rather than nesting
    // the size under a key nobody would guess.
    assert!(json.contains(r#""width":1280"#), "{json}");
    assert!(json.contains(r#""channel":"c1""#), "{json}");
    assert_eq!(serde_json::from_str::<Restored>(&json).unwrap(), left);
    assert_eq!(left.clone().sane(), left);

    // A file written before the channel was remembered.
    let older: Restored =
        serde_json::from_str(r#"{"width":1280,"height":1400,"maximized":false}"#).unwrap();
    assert_eq!(older.geometry.width, 1_280);
    assert!(older.channel.is_empty());

    // And an unusable size is still replaced, channel kept.
    let broken = Restored {
        geometry: Geometry {
            width: 1,
            height: 1,
            maximized: false,
        },
        channel: "c2".to_owned(),
    }
    .sane();
    assert_eq!(broken.geometry, Geometry::default());
    assert_eq!(broken.channel, "c2");
    assert!(!format!("{broken:?}").is_empty());
    assert_eq!(Restored::default().channel, "");
}
