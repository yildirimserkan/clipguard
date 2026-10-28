// App icon, drawn in code (no binary assets): dark rounded square, green `>_` prompt.

const BG: [u8; 3] = [0x0b, 0x0e, 0x14];
const BORDER: [u8; 3] = [0x30, 0x36, 0x3d];
const GREEN: [u8; 3] = [0x7e, 0xe7, 0x87];

fn dist_to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / (abx * abx + aby * aby)).clamp(0.0, 1.0);
    let (cx, cy) = (a.0 + t * abx, a.1 + t * aby);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}

/// Signed distance to a rounded square covering the whole canvas (negative = inside).
fn rounded_rect_sd(p: (f32, f32), size: f32, r: f32) -> f32 {
    let h = size / 2.0;
    let (qx, qy) = ((p.0 - h).abs() - (h - r), (p.1 - h).abs() - (h - r));
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

/// Returns (rgba, width, height) for a square icon of `size` px.
pub fn rgba(size: u32) -> (Vec<u8>, u32, u32) {
    let s = size as f32;
    let k = s / 32.0; // design grid is 32x32
    let stroke = 1.6 * k;
    let chevron = [((8.0 * k, 9.5 * k), (15.5 * k, 16.0 * k)), ((15.5 * k, 16.0 * k), (8.0 * k, 22.5 * k))];
    let underscore = ((18.0 * k, 22.5 * k), (24.5 * k, 22.5 * k));

    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let sd = rounded_rect_sd(p, s, 6.0 * k);
            let alpha = (0.5 - sd).clamp(0.0, 1.0); // anti-aliased edge
            let mut c = if sd > -1.2 * k { BORDER } else { BG };

            let d = chevron
                .iter()
                .map(|(a, b)| dist_to_segment(p, *a, *b))
                .fold(dist_to_segment(p, underscore.0, underscore.1), f32::min);
            c = mix(c, GREEN, stroke - d + 0.5);

            out.extend_from_slice(&[c[0], c[1], c[2], (alpha * 255.0).round() as u8]);
        }
    }
    (out, size, size)
}

#[cfg(test)]
mod tests {
    #[test]
    fn icon_shape() {
        let (px, w, h) = super::rgba(32);
        assert_eq!((px.len(), w, h), (32 * 32 * 4, 32, 32));
        assert_eq!(px[3], 0, "corner is transparent");
        let center = ((16 * 32 + 16) * 4) as usize;
        assert_eq!(px[center + 3], 255, "center is opaque");
    }
}
