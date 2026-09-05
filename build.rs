include!("src/icon_pixels.rs");

use std::{env, fs, io, path::Path};

fn main() {
    println!("cargo:rerun-if-changed=src/icon_pixels.rs");
    if env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }

    let output = env::var_os("OUT_DIR").expect("OUT_DIR is not set");
    let icon_path = Path::new(&output).join("tuhai-view.ico");
    write_ico(&icon_path, &[16, 24, 32, 48, 64, 128, 256])
        .expect("failed to generate application icon");

    let mut resource = winres::WindowsResource::new();
    resource.set_icon(icon_path.to_string_lossy().as_ref());
    resource.set("ProductName", "图海速览");
    resource.set("FileDescription", "图海速览：大规模图片浏览与整理工具");
    resource.set("ProductVersion", "20260905");
    resource.set("FileVersion", "2026.9.5.0");
    resource.set("OriginalFilename", "TuHaiView.exe");
    resource
        .compile()
        .expect("failed to embed Windows application icon");
}

fn write_ico(path: &Path, sizes: &[u32]) -> io::Result<()> {
    let images = sizes
        .iter()
        .map(|&size| (size, dib_image(size)))
        .collect::<Vec<_>>();
    let directory_size = 6 + images.len() * 16;
    let total_size = directory_size + images.iter().map(|(_, image)| image.len()).sum::<usize>();
    let mut ico = Vec::with_capacity(total_size);

    push_u16(&mut ico, 0);
    push_u16(&mut ico, 1);
    push_u16(&mut ico, images.len() as u16);

    let mut offset = directory_size as u32;
    for (size, image) in &images {
        ico.push(if *size == 256 { 0 } else { *size as u8 });
        ico.push(if *size == 256 { 0 } else { *size as u8 });
        ico.push(0);
        ico.push(0);
        push_u16(&mut ico, 1);
        push_u16(&mut ico, 32);
        push_u32(&mut ico, image.len() as u32);
        push_u32(&mut ico, offset);
        offset += image.len() as u32;
    }

    for (_, image) in images {
        ico.extend_from_slice(&image);
    }
    fs::write(path, ico)
}

fn dib_image(size: u32) -> Vec<u8> {
    let rgba = app_icon_rgba(size);
    let mask_row_bytes = size.div_ceil(32) * 4;
    let pixel_bytes = size * size * 4;
    let mask_bytes = mask_row_bytes * size;
    let mut dib = Vec::with_capacity((40 + pixel_bytes + mask_bytes) as usize);

    push_u32(&mut dib, 40);
    push_i32(&mut dib, size as i32);
    push_i32(&mut dib, (size * 2) as i32);
    push_u16(&mut dib, 1);
    push_u16(&mut dib, 32);
    push_u32(&mut dib, 0);
    push_u32(&mut dib, pixel_bytes);
    push_i32(&mut dib, 0);
    push_i32(&mut dib, 0);
    push_u32(&mut dib, 0);
    push_u32(&mut dib, 0);

    for y in (0..size).rev() {
        for x in 0..size {
            let offset = ((y * size + x) * 4) as usize;
            dib.push(rgba[offset + 2]);
            dib.push(rgba[offset + 1]);
            dib.push(rgba[offset]);
            dib.push(rgba[offset + 3]);
        }
    }
    dib.resize((40 + pixel_bytes + mask_bytes) as usize, 0);
    dib
}

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_i32(output: &mut Vec<u8>, value: i32) {
    output.extend_from_slice(&value.to_le_bytes());
}
