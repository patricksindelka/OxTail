//! The window icon, drawn procedurally so no asset file is needed at runtime.

/// Edge length of the icon in pixels.
pub const ICON_SIZE: u32 = 64;

/// RGBA pixels of the icon: a rounded dark tile with "log lines" (one of them
/// an error line) and a green cursor block, a nod to `tail -f`.
pub fn icon_rgba() -> Vec<u8> {
    let n = ICON_SIZE as usize;
    let mut px = vec![0u8; n * n * 4];
    let mut put = |x: usize, y: usize, c: [u8; 4]| {
        let i = (y * n + x) * 4;
        px[i..i + 4].copy_from_slice(&c);
    };
    // Rounded tile.
    let radius = 12.0f32;
    let bg = [0x1e, 0x2a, 0x3a, 0xff];
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let cx = fx.clamp(radius, n as f32 - radius);
            let cy = fy.clamp(radius, n as f32 - radius);
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            if d <= radius {
                put(x, y, bg);
            }
        }
    }
    // Log lines: (y, width, colour).
    let lines: [(usize, usize, [u8; 4]); 4] = [
        (14, 40, [0x9a, 0xa5, 0xb8, 0xff]),
        (24, 30, [0x9a, 0xa5, 0xb8, 0xff]),
        (34, 44, [0xf7, 0x76, 0x8e, 0xff]),
        (44, 22, [0x9a, 0xa5, 0xb8, 0xff]),
    ];
    for (y, w, c) in lines {
        for yy in y..y + 5 {
            for x in 10..10 + w {
                put(x, yy, c);
            }
        }
    }
    // Cursor block after the last line.
    for y in 43..50 {
        for x in 36..42 {
            put(x, y, [0x9e, 0xce, 0x6a, 0xff]);
        }
    }
    px
}

/// The icon as egui icon data.
pub fn icon_data() -> egui::IconData {
    egui::IconData {
        rgba: icon_rgba(),
        width: ICON_SIZE,
        height: ICON_SIZE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_has_the_right_size_and_is_not_blank() {
        let d = icon_data();
        assert_eq!(d.rgba.len(), (d.width * d.height * 4) as usize);
        let opaque = d.rgba.chunks(4).filter(|p| p[3] == 0xff).count();
        assert!(opaque > 1000);
        // Corners are transparent (rounded tile).
        assert_eq!(d.rgba[3], 0);
    }
}
