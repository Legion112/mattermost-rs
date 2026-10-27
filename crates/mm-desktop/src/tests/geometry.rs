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
