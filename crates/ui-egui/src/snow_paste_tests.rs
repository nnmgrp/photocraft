use photocraft_raster::Surface;

fn gray_surface(width: i32, height: i32, pixels: &[f32]) -> Surface {
    let mut surface = Surface::new(photocraft_color::PixelFormat::GRAY8);
    surface.write_region(photocraft_geom::Rect::new(0, 0, width, height), pixels);
    surface
}

#[test]
fn gray8_selection_changes_paste_fingerprint_and_limits_cleanup_sampling() {
    let selection = gray_surface(
        4,
        4,
        &[
            0.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 1.0, 0.0, //
            0.0, 1.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 0.0,
        ],
    );
    let other_selection = gray_surface(
        4,
        4,
        &[
            0.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 1.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 0.0,
        ],
    );
    let artwork = gray_surface(
        4,
        4,
        &[
            0.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 1.0, 0.0, //
            0.0, 1.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 0.0,
        ],
    );

    assert_eq!(selection.content_bounds(), other_selection.content_bounds());
    assert_ne!(crate::snow_clean::selection_fingerprint(&selection), crate::snow_clean::selection_fingerprint(&other_selection));
    assert_eq!(crate::snow_clean::mean_luminance(Some(&artwork), &selection), Some(255.0));
    assert_eq!(crate::snow_clean::clean_fill_color(Some(&artwork), &selection), [255, 255, 255]);
}
