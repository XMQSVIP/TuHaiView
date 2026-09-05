const SAMPLES: u32 = 4;

pub fn app_icon_rgba(size: u32) -> Vec<u8> {
    assert!(size > 0);
    let mut rgba = vec![0_u8; size as usize * size as usize * 4];
    let sample_count = SAMPLES * SAMPLES;

    for y in 0..size {
        for x in 0..size {
            let mut sum = [0_u32; 4];
            for sample_y in 0..SAMPLES {
                for sample_x in 0..SAMPLES {
                    let px = (x as f32 + (sample_x as f32 + 0.5) / SAMPLES as f32) / size as f32;
                    let py = (y as f32 + (sample_y as f32 + 0.5) / SAMPLES as f32) / size as f32;
                    let color = sample_icon(px, py);
                    for channel in 0..4 {
                        sum[channel] += color[channel] as u32;
                    }
                }
            }

            let offset = (y as usize * size as usize + x as usize) * 4;
            for channel in 0..4 {
                rgba[offset + channel] = (sum[channel] / sample_count) as u8;
            }
        }
    }

    rgba
}

fn sample_icon(x: f32, y: f32) -> [u8; 4] {
    if !inside_rounded_rect(x, y, 0.03, 0.03, 0.97, 0.97, 0.22) {
        return [0, 0, 0, 0];
    }

    let blend = ((x + y) * 0.5).clamp(0.0, 1.0);
    let mut color = mix([32, 126, 245, 255], [116, 52, 214, 255], blend);

    if inside_rounded_rect(x, y, 0.15, 0.17, 0.85, 0.83, 0.075) {
        color = [250, 252, 255, 255];
    }

    if inside_rounded_rect(x, y, 0.21, 0.24, 0.79, 0.70, 0.025) {
        color = [111, 210, 246, 255];
    }

    if circle(x, y, 0.65, 0.37, 0.082) {
        color = [255, 205, 64, 255];
    }

    if point_in_triangle(x, y, (0.21, 0.70), (0.53, 0.38), (0.79, 0.70)) {
        color = [37, 171, 158, 255];
    }

    if point_in_triangle(x, y, (0.21, 0.70), (0.38, 0.49), (0.59, 0.70)) {
        color = [22, 116, 150, 255];
    }

    color
}

fn inside_rounded_rect(
    x: f32,
    y: f32,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    radius: f32,
) -> bool {
    if x < left || x > right || y < top || y > bottom {
        return false;
    }
    let center_x = x.clamp(left + radius, right - radius);
    let center_y = y.clamp(top + radius, bottom - radius);
    let dx = x - center_x;
    let dy = y - center_y;
    dx * dx + dy * dy <= radius * radius
}

fn circle(x: f32, y: f32, center_x: f32, center_y: f32, radius: f32) -> bool {
    let dx = x - center_x;
    let dy = y - center_y;
    dx * dx + dy * dy <= radius * radius
}

fn point_in_triangle(x: f32, y: f32, a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let sign = |p: (f32, f32), p1: (f32, f32), p2: (f32, f32)| {
        (p.0 - p2.0) * (p1.1 - p2.1) - (p1.0 - p2.0) * (p.1 - p2.1)
    };
    let point = (x, y);
    let d1 = sign(point, a, b);
    let d2 = sign(point, b, c);
    let d3 = sign(point, c, a);
    let has_negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_negative && has_positive)
}

fn mix(from: [u8; 4], to: [u8; 4], amount: f32) -> [u8; 4] {
    let mut result = [0_u8; 4];
    for channel in 0..4 {
        result[channel] =
            (from[channel] as f32 * (1.0 - amount) + to[channel] as f32 * amount) as u8;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_has_expected_size_and_transparent_corners() {
        let icon = app_icon_rgba(64);
        assert_eq!(icon.len(), 64 * 64 * 4);
        assert_eq!(icon[3], 0);
        let center = (32 * 64 + 32) * 4;
        assert_eq!(icon[center + 3], 255);
    }
}
