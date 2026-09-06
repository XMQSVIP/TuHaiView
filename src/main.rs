#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod budget;
mod catalog;
mod decoding;
mod disk_profile;
mod duplicates;
mod empty_folders;
mod file_ops;
mod gpu_images;
mod heap_diagnostics;
mod icon_pixels;
mod models;
#[cfg(test)]
mod perf_tests;
mod performance;
mod retirement;
mod sorting;
mod storage;
mod thumbnail_cache;
mod thumbnails;

use anyhow::Result;
use eframe::egui;

pub(crate) const APP_NAME: &str = "图海速览";
pub(crate) const APP_VERSION: &str = "20260905";
pub(crate) const APP_WINDOW_TITLE: &str = "图海速览 20260905";

fn main() -> Result<()> {
    performance::initialize_clock();
    tracing_subscriber::fmt().with_env_filter("info").init();
    let _timer_resolution = performance::TimerResolution::diagnostic();
    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    // DX12 exposes Mailbox: retain the newest frame without tearing or waiting
    // for every queued FIFO frame. Idle windows still rely on event-driven repaint.
    wgpu_options.present_mode = wgpu::PresentMode::Mailbox;
    if std::env::var("TUHAI_PERF").ok().as_deref() == Some("1") {
        let on_error = wgpu_options.on_surface_error.clone();
        wgpu_options.on_surface_error = std::sync::Arc::new(move |error| {
            performance::sample(
                match error {
                    wgpu::SurfaceError::Timeout => "surface_timeout",
                    wgpu::SurfaceError::Outdated => "surface_outdated",
                    wgpu::SurfaceError::Lost => "surface_lost",
                    wgpu::SurfaceError::OutOfMemory => "surface_out_of_memory",
                    wgpu::SurfaceError::Other => "surface_other_error",
                },
                1.0,
            );
            on_error(error)
        });
        if std::env::var("TUHAI_PERF_PRESENT").ok().as_deref() == Some("immediate") {
            wgpu_options.present_mode = wgpu::PresentMode::AutoNoVsync;
        }
        if std::env::var("TUHAI_PERF_PRESENT").ok().as_deref() == Some("vsync") {
            wgpu_options.present_mode = wgpu::PresentMode::AutoVsync;
        }
        if let Ok(value) = std::env::var("TUHAI_PERF_LATENCY") {
            wgpu_options.desired_maximum_frame_latency = value.parse().ok();
        }
    }
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
    let application = eframe::run_native(
        APP_WINDOW_TITLE,
        options,
        Box::new(|cc| Ok(Box::new(app::PreviewerApp::new(cc)?))),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()));
    let finalized = performance::finalize_after_window();
    if performance::enabled() {
        eprintln!("TUHAI_FINALIZE {}", serde_json::to_string(&finalized)?);
    }
    if !finalized.succeeded() {
        anyhow::bail!("performance log finalization failed");
    }
    application
}
