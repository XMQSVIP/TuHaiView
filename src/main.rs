#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod catalog;
mod duplicates;
mod empty_folders;
mod file_ops;
mod icon_pixels;
mod models;
mod sorting;
mod storage;
mod thumbnails;

use anyhow::Result;
use eframe::egui;

pub(crate) const APP_NAME: &str = "图海速览";
pub(crate) const APP_VERSION: &str = "20260905";
pub(crate) const APP_WINDOW_TITLE: &str = "图海速览 20260905";

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    let mut wgpu_setup = eframe::egui_wgpu::WgpuSetupCreateNew::default();
    // The default backend selection used OpenGL on some Intel systems. The
    // affected ig9icd64.dll driver crashes while tearing down the window.
    // DX12 avoids that driver path and is available on supported Windows 10/11.
    wgpu_setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
    wgpu_options.wgpu_setup = eframe::egui_wgpu::WgpuSetup::CreateNew(wgpu_setup);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_WINDOW_TITLE)
            .with_icon(egui::IconData {
                rgba: icon_pixels::app_icon_rgba(64),
                width: 64,
                height: 64,
            })
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([900.0, 600.0]),
        wgpu_options,
        ..Default::default()
    };
    eframe::run_native(
        APP_WINDOW_TITLE,
        options,
        Box::new(|cc| Ok(Box::new(app::PreviewerApp::new(cc)?))),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))
}
