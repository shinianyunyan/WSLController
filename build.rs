use std::{env, fs, path::PathBuf};

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let icon_path = out_dir.join("wslcontroller.ico");
    write_icon(&icon_path);

    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon(icon_path.to_string_lossy().as_ref());
        res.set("FileDescription", "WSLController");
        res.set("ProductName", "WSLController");
        res.set("OriginalFilename", "WSLController.exe");
        res.set("LegalCopyright", "Copyright (c) 2026 shinianyunyan");
        res.compile().expect("compile Windows resources");
    }

    println!("cargo:rerun-if-changed=build.rs");
}

fn write_icon(path: &PathBuf) {
    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in [16, 24, 32, 48, 64, 128, 256] {
        let (rgba, width, height) = icon_pixels(size);
        let image = ico::IconImage::from_rgba_data(width, height, rgba);
        dir.add_entry(ico::IconDirEntry::encode(&image).expect("encode icon entry"));
    }

    let mut file = fs::File::create(path).expect("create generated icon");
    dir.write(&mut file).expect("write generated icon");
}

fn icon_pixels(size: u32) -> (Vec<u8>, u32, u32) {
    let mut rgba = vec![0_u8; (size * size * 4) as usize];
    let scale = size as f32 / 58.0;

    for y in 0..size {
        for x in 0..size {
            let idx = ((y * size + x) * 4) as usize;
            let xf = x as f32 / scale;
            let yf = y as f32 / scale;

            let outer_alpha = rounded_rect_alpha(xf, yf, 0.0, 0.0, 58.0, 58.0, 9.0);
            if outer_alpha <= 0.0 {
                continue;
            }

            let inner_alpha = rounded_rect_alpha(xf, yf, 6.0, 6.0, 46.0, 46.0, 5.0);
            let mut color = if inner_alpha > 0.0 {
                mix(
                    [39, 114, 159, 255],
                    [16, 18, 24, 255],
                    inner_alpha,
                )
            } else {
                [39, 114, 159, 255]
            };

            let prompt_alpha = polyline_alpha(
                xf,
                yf,
                &[(17.0, 20.0), (27.0, 29.0), (17.0, 38.0)],
                2.7,
            );
            if prompt_alpha > 0.0 {
                color = mix(color, [95, 211, 167, 255], prompt_alpha);
            }

            let cursor_alpha = segment_alpha(xf, yf, (33.0, 40.0), (45.0, 40.0), 2.9);
            if cursor_alpha > 0.0 {
                color = mix(color, [230, 238, 245, 255], cursor_alpha);
            }

            color[3] = (outer_alpha * 255.0).round() as u8;
            rgba[idx..idx + 4].copy_from_slice(&color);
        }
    }

    (rgba, size, size)
}

fn rounded_rect_alpha(
    x: f32,
    y: f32,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    radius: f32,
) -> f32 {
    let right = left + width;
    let bottom = top + height;
    let closest_x = x.clamp(left + radius, right - radius);
    let closest_y = y.clamp(top + radius, bottom - radius);
    let dx = x - closest_x;
    let dy = y - closest_y;
    (radius + 0.75 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0)
}

fn polyline_alpha(x: f32, y: f32, points: &[(f32, f32)], width: f32) -> f32 {
    points
        .windows(2)
        .map(|pair| segment_alpha(x, y, pair[0], pair[1], width))
        .fold(0.0, f32::max)
}

fn segment_alpha(x: f32, y: f32, start: (f32, f32), end: (f32, f32), width: f32) -> f32 {
    let (x1, y1) = start;
    let (x2, y2) = end;
    let vx = x2 - x1;
    let vy = y2 - y1;
    let len_sq = vx * vx + vy * vy;
    if len_sq <= f32::EPSILON {
        return 0.0;
    }

    let t = (((x - x1) * vx + (y - y1) * vy) / len_sq).clamp(0.0, 1.0);
    let px = x1 + t * vx;
    let py = y1 + t * vy;
    let distance = ((x - px).powi(2) + (y - py).powi(2)).sqrt();
    ((width / 2.0 + 0.65 - distance).clamp(0.0, 1.0)).min(1.0)
}

fn mix(base: [u8; 4], overlay: [u8; 4], alpha: f32) -> [u8; 4] {
    let a = alpha.clamp(0.0, 1.0);
    [
        lerp_u8(base[0], overlay[0], a),
        lerp_u8(base[1], overlay[1], a),
        lerp_u8(base[2], overlay[2], a),
        lerp_u8(base[3], overlay[3], a),
    ]
}

fn lerp_u8(from: u8, to: u8, alpha: f32) -> u8 {
    (from as f32 + (to as f32 - from as f32) * alpha).round() as u8
}
