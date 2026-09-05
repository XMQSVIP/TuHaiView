//! Manual, isolated renderer diagnostic. Never part of the portable product.
//! TUHAI_PROBE_LOG is required; TUHAI_PROBE_SECONDS defaults to 180.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use eframe::{egui, egui_wgpu::RenderState};
use std::{
    fs::File,
    io::{BufWriter, Write},
    time::{Duration, Instant},
};

struct Probe {
    started: Instant,
    sampled: Instant,
    previous: Instant,
    frames: u64,
    seconds: f64,
    idle_seconds: f64,
    delay: Duration,
    extra_poll: bool,
    no_widgets: bool,
    buffer_probe: Option<BufferProbe>,
    renderer: RenderState,
    log: BufWriter<File>,
}

struct BufferProbe {
    target: wgpu::Buffer,
    bytes: Vec<u8>,
    belt: Option<wgpu::util::StagingBelt>,
}
impl BufferProbe {
    fn upload(&mut self, state: &RenderState) {
        if let Some(belt) = self.belt.as_mut() {
            let mut encoder = state.device.create_command_encoder(&Default::default());
            for offset in [0, self.bytes.len() as u64] {
                belt.write_buffer(
                    &mut encoder,
                    &self.target,
                    offset,
                    std::num::NonZeroU64::new(self.bytes.len() as u64).unwrap(),
                    &state.device,
                )
                .copy_from_slice(&self.bytes);
            }
            belt.finish();
            state.queue.submit([encoder.finish()]);
            belt.recall();
        } else {
            for offset in [0, self.bytes.len() as u64] {
                state.queue.write_buffer(&self.target, offset, &self.bytes);
            }
        }
    }
}

fn memory() -> Option<(usize, usize)> {
    use windows::Win32::System::{
        ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
        },
        Threading::GetCurrentProcess,
    };
    let mut value = PROCESS_MEMORY_COUNTERS_EX::default();
    value.cb = std::mem::size_of_val(&value) as u32;
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut value as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            value.cb,
        )
        .ok()?;
    }
    Some((value.PrivateUsage, value.WorkingSetSize))
}

impl Probe {
    fn record(&mut self, value: serde_json::Value) {
        serde_json::to_writer(&mut self.log, &value).expect("write diagnostic log");
        self.log.write_all(b"\n").expect("write diagnostic newline");
    }
}
impl eframe::App for Probe {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        let now = Instant::now();
        let interval = now.duration_since(self.previous).as_secs_f64() * 1000.0;
        self.previous = now;
        self.frames += 1;
        let elapsed = self.started.elapsed().as_secs_f64();
        let idle = elapsed >= self.seconds;
        if self.extra_poll {
            let _ = self.renderer.device.poll(wgpu::Maintain::Poll);
        }
        if !idle {
            if let Some(probe) = self.buffer_probe.as_mut() {
                probe.upload(&self.renderer);
            }
        }
        // Constant widgets: no images, catalog, decoder, font-file loading or cache.
        if !self.no_widgets {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.heading("DX12 isolated renderer diagnostic");
                ui.label("No catalog, images or background work");
            });
        }
        if self.sampled.elapsed() >= Duration::from_secs(1) {
            self.sampled = now;
            let native = self.renderer.device.generate_allocator_report();
            // Build with --features wgpu/counters for native object counts.
            // Without that opt-in these API counters intentionally report zero.
            let counters = self.renderer.device.get_internal_counters().hal;
            let usage = memory();
            self.record(serde_json::json!({
                "seconds":elapsed,"frames":self.frames,"last_interval_ms":interval,
                "private_bytes":usage.map(|v| v.0),"working_set_bytes":usage.map(|v| v.1),
                "native_allocated":native.as_ref().map(|r| r.total_allocated_bytes),
                "native_reserved":native.as_ref().map(|r| r.total_reserved_bytes),
                "native_allocations":native.as_ref().map(|r| r.allocations.len()),
                "hal_buffers":counters.buffers.read(),
                "hal_textures":counters.textures.read(),
                "hal_texture_views":counters.texture_views.read(),
                "hal_bind_groups":counters.bind_groups.read(),
                "hal_command_encoders":counters.command_encoders.read(),
                "hal_fences":counters.fences.read(),
                "hal_buffer_memory":counters.buffer_memory.read(),
                "hal_texture_memory":counters.texture_memory.read(),
                "idle":idle,
                "pixels_per_point":ctx.pixels_per_point(),
                "window_size":ctx.input(|i| [i.screen_rect().width(),i.screen_rect().height()]),
                "window_minimized":ctx.input(|i| i.viewport().minimized.unwrap_or(false)),
            }));
            self.log.flush().expect("flush diagnostic log");
        }
        if elapsed >= self.seconds + self.idle_seconds {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if idle {
            ctx.request_repaint_after(Duration::from_secs(1));
        } else if self.delay.is_zero() {
            ctx.request_repaint();
        } else {
            let predicted = ctx.input(|i| Duration::from_secs_f32(i.predicted_dt.max(0.0)));
            ctx.request_repaint_after(self.delay + predicted);
        }
    }
    fn on_exit(&mut self) {
        let elapsed = self.started.elapsed().as_secs_f64();
        self.record(
            serde_json::json!({"completed_seconds":elapsed,"frames":self.frames,
            "completed":elapsed >= self.seconds + self.idle_seconds}),
        );
        self.log.flush().expect("flush final diagnostic log");
        self.log
            .get_ref()
            .sync_all()
            .expect("sync final diagnostic log");
    }
}

fn main() -> Result<(), eframe::Error> {
    let path = std::env::var_os("TUHAI_PROBE_LOG").expect("Set an explicit TUHAI_PROBE_LOG path");
    let file = File::create(path).expect("create diagnostic log");
    let seconds = std::env::var("TUHAI_PROBE_SECONDS")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(180.0)
        .clamp(10.0, 1800.0);
    let delay_ms = std::env::var("TUHAI_PROBE_REPAINT_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let idle_seconds = std::env::var("TUHAI_PROBE_IDLE_SECONDS")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(30.0)
        .clamp(0.0, 300.0);
    let extra_poll = std::env::var("TUHAI_PROBE_EXTRA_POLL").ok().as_deref() == Some("1");
    let no_widgets = std::env::var("TUHAI_PROBE_NO_WIDGETS").ok().as_deref() == Some("1");
    let buffer_mode = std::env::var("TUHAI_PROBE_BUFFER_MODE").unwrap_or_default();
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::default();
    setup.instance_descriptor.backends = wgpu::Backends::DX12;
    if std::env::var("TUHAI_PROBE_WARP").ok().as_deref() == Some("1") {
        setup.native_adapter_selector = Some(std::sync::Arc::new(|adapters, surface| {
            adapters
                .iter()
                .find(|adapter| {
                    adapter.get_info().device_type == wgpu::DeviceType::Cpu
                        && surface.is_none_or(|s| adapter.is_surface_supported(s))
                })
                .cloned()
                .ok_or_else(|| "No compatible software DX12 adapter".into())
        }));
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 820.0]),
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            present_mode: if std::env::var("TUHAI_PROBE_FIFO").ok().as_deref() == Some("1") {
                wgpu::PresentMode::Fifo
            } else {
                wgpu::PresentMode::Mailbox
            },
            wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(setup),
            ..Default::default()
        },
        ..Default::default()
    };
    eframe::run_native(
        "DX12 memory diagnostic",
        options,
        Box::new(move |cc| {
            let renderer = cc.wgpu_render_state.clone().expect("DX12 renderer");
            let buffer_probe = if matches!(buffer_mode.as_str(), "queue" | "belt") {
                Some(BufferProbe {
                    target: renderer.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("isolated buffer upload target"),
                        size: 128 * 1024,
                        usage: wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }),
                    bytes: vec![42; 64 * 1024],
                    belt: (buffer_mode == "belt").then(|| wgpu::util::StagingBelt::new(256 * 1024)),
                })
            } else {
                None
            };
            let mut probe = Probe {
                started: Instant::now(),
                sampled: Instant::now(),
                previous: Instant::now(),
                frames: 0,
                seconds,
                idle_seconds,
                delay: Duration::from_millis(delay_ms),
                extra_poll,
                no_widgets,
                buffer_probe,
                renderer,
                log: BufWriter::new(file),
            };
            probe.record(serde_json::json!({"kind":"probe_header","pid":std::process::id(),"adapter":format!("{:?}",probe.renderer.adapter.get_info()),"seconds":seconds,"idle_seconds":idle_seconds,"repaint_ms":delay_ms,"extra_poll":extra_poll,"no_widgets":no_widgets,"buffer_mode":buffer_mode,
                "present":if std::env::var("TUHAI_PROBE_FIFO").ok().as_deref()==Some("1"){"fifo"}else{"mailbox"}}));
            Ok(Box::new(probe))
        }),
    )
}
