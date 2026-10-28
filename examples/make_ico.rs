// Regenerates assets/clipguard.ico from the in-code icon:
//   cargo run --example make_ico
// ICO with 16/24/32/48/64/256 px entries, 32-bit BGRA DIB + AND mask.
#[allow(dead_code)]
mod icon {
    include!("../src/icon.rs");
}

fn dib(size: u32) -> Vec<u8> {
    let (rgba, w, h) = icon::rgba(size);
    let mut out = Vec::new();
    // BITMAPINFOHEADER (height doubled: XOR + AND mask)
    for v in [40u32, w, h * 2] { out.extend_from_slice(&v.to_le_bytes()); }
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bpp
    for _ in 0..6 { out.extend_from_slice(&0u32.to_le_bytes()); }
    // XOR bitmap: bottom-up BGRA
    for y in (0..h).rev() {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            out.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    // AND mask: all zero (alpha channel decides), rows padded to 32 bits
    let row = ((w + 31) / 32 * 4) as usize;
    out.extend(std::iter::repeat(0u8).take(row * h as usize));
    out
}

fn main() {
    let sizes = [16u32, 24, 32, 48, 64, 256];
    let images: Vec<Vec<u8>> = sizes.iter().map(|&s| dib(s)).collect();
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (s, img) in sizes.iter().zip(&images) {
        let b = if *s >= 256 { 0 } else { *s as u8 };
        ico.extend_from_slice(&[b, b, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&(img.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in &images { ico.extend_from_slice(img); }
    std::fs::write("assets/clipguard.ico", &ico).expect("write ico");
    println!("assets/clipguard.ico: {} bytes, sizes {:?}", ico.len(), sizes);
}
