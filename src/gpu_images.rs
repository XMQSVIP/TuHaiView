//! Native texture uploads are sliced by bytes; an incomplete texture is never displayed.
use crate::{
    budget::{ByteBudget, ByteLease},
    performance,
    thumbnails::{ImageResult, ThumbnailService},
};
use eframe::{egui, egui_wgpu::RenderState};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

static GPU_RETIRED: AtomicUsize = AtomicUsize::new(0);
#[derive(Clone)]
pub struct GpuImage(Arc<ImageInner>);
struct ImageInner {
    id: egui::TextureId,
    size: [usize; 2],
    // Ownership keeps the native allocation alive; tests additionally read it back.
    #[allow(dead_code)]
    texture: wgpu::Texture,
    state: RenderState,
    pub requested_side: u32,
    allocation: Option<ByteLease>,
}
impl Drop for ImageInner {
    fn drop(&mut self) {
        let started = Instant::now();
        self.state.renderer.write().free_texture(&self.id);
        if let Some(allocation) = self.allocation.take() {
            let texture = self.texture.clone();
            let bytes = self.size[0] * self.size[1] * 4;
            GPU_RETIRED.fetch_add(bytes, Ordering::AcqRel);
            self.state.queue.on_submitted_work_done(move || {
                drop(texture);
                drop(allocation);
                GPU_RETIRED.fetch_sub(bytes, Ordering::AcqRel);
            });
        }
        performance::elapsed("texture_unregister_ms", started);
    }
}
impl GpuImage {
    pub fn size(&self) -> [usize; 2] {
        self.0.size
    }
    pub fn size_vec2(&self) -> egui::Vec2 {
        egui::vec2(self.0.size[0] as f32, self.0.size[1] as f32)
    }
    pub fn id(&self) -> egui::TextureId {
        self.0.id
    }
    pub fn requested_side(&self) -> u32 {
        self.0.requested_side
    }
}
struct Pending {
    result: ImageResult,
    texture: wgpu::Texture,
    row: usize,
    allocation: ByteLease,
}
pub struct Uploads {
    state: RenderState,
    pending: Option<Pending>,
    retired: VecDeque<GpuImage>,
    retired_pending: Vec<Pending>,
    budget: Arc<ByteBudget>,
    allocator_report_due: Instant,
    allocator_diagnostics: bool,
}
impl Uploads {
    pub fn new(state: RenderState) -> Self {
        if performance::enabled() {
            tracing::info!(adapter = ?state.adapter.get_info(), "selected renderer adapter");
        }
        Self {
            state,
            pending: None,
            retired: VecDeque::new(),
            retired_pending: Vec::new(),
            budget: ByteBudget::new(performance::TEXTURE_BYTES, 0),
            allocator_report_due: Instant::now(),
            allocator_diagnostics: performance::enabled()
                && std::env::var("TUHAI_PERF_ALLOCATOR").ok().as_deref() == Some("1"),
        }
    }
    pub fn used_bytes(&self) -> usize {
        self.budget.used()
    }
    pub fn retire(&mut self, image: GpuImage) {
        self.retired.push_back(image);
    }
    pub fn reclaim(&mut self) {
        let started = Instant::now();
        // Previous frame has been submitted by eframe before this update begins.
        for p in self.retired_pending.drain(..) {
            let bytes = p.result.pixels.len();
            crate::retirement::retire(p.result, bytes);
            GPU_RETIRED.fetch_add(bytes, Ordering::AcqRel);
            self.state.queue.on_submitted_work_done(move || {
                drop(p.texture);
                drop(p.allocation);
                GPU_RETIRED.fetch_sub(bytes, Ordering::AcqRel);
            });
        }
        for _ in 0..8 {
            if started.elapsed() >= Duration::from_millis(performance::EVENT_BUDGET_MS) {
                break;
            }
            let Some(image) = self.retired.pop_front() else {
                break;
            };
            drop(image);
        }
        let _ = self.state.device.poll(wgpu::Maintain::Poll);
        if self.allocator_diagnostics && Instant::now() >= self.allocator_report_due {
            self.allocator_report_due = Instant::now() + Duration::from_secs(5);
            let start = Instant::now();
            crate::heap_diagnostics::record();
            if let Some(report) = self.state.device.generate_allocator_report() {
                performance::sample(
                    "wgpu_allocator_allocated_bytes",
                    report.total_allocated_bytes as f64,
                );
                performance::sample(
                    "wgpu_allocator_reserved_bytes",
                    report.total_reserved_bytes as f64,
                );
                performance::sample(
                    "wgpu_allocator_allocations",
                    report.allocations.len() as f64,
                );
                performance::sample("wgpu_allocator_blocks", report.blocks.len() as f64);
            }
            performance::elapsed("wgpu_allocator_report_ms", start);
        }
        performance::gauge("gpu_allocated_bytes", self.budget.used() as f64);
        performance::gauge(
            "gpu_retired_bytes",
            GPU_RETIRED.load(Ordering::Acquire) as f64,
        );
        performance::gauge(
            "texture_retired_count",
            (self.retired.len() + self.retired_pending.len()) as f64,
        );
    }
    pub fn needs_reclaim(&self) -> bool {
        !self.retired.is_empty()
            || !self.retired_pending.is_empty()
            || GPU_RETIRED.load(Ordering::Acquire) > 0
    }
    pub fn pending_bytes(&self) -> usize {
        self.pending
            .as_ref()
            .map_or(0, |p| p.result.width * p.result.height * 4)
    }
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn clear(&mut self, service: &ThumbnailService) {
        if let Some(p) = self.pending.take() {
            service.acknowledge(&p.result);
            self.retired_pending.push(p);
        }
    }
    pub fn queue(&mut self, result: ImageResult) -> Result<(), ImageResult> {
        debug_assert!(self.pending.is_none());
        let Some(allocation) = self.budget.try_acquire(result.pixels.len()) else {
            return Err(result);
        };
        let started = Instant::now();
        let texture = self.state.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bounded image upload"),
            size: wgpu::Extent3d {
                width: result.width as u32,
                height: result.height as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::TEXTURE_BINDING
                | if cfg!(test) {
                    wgpu::TextureUsages::COPY_SRC
                } else {
                    wgpu::TextureUsages::empty()
                },
            view_formats: &[],
        });
        self.pending = Some(Pending {
            result,
            texture,
            row: 0,
            allocation,
        });
        performance::elapsed("texture_create_ms", started);
        Ok(())
    }
    pub fn advance(
        &mut self,
        service: &ThumbnailService,
        remaining: &mut usize,
        started: Instant,
    ) -> Option<(ImageResult, GpuImage)> {
        let pending = self.pending.as_mut()?;
        if !service.is_current(&pending.result) {
            self.clear(service);
            return None;
        }
        while pending.row < pending.result.height
            && *remaining >= pending.result.width * 4
            && started.elapsed() < Duration::from_millis(performance::UPLOAD_BUDGET_MS)
        {
            let rows =
                (*remaining / (pending.result.width * 4)).min(pending.result.height - pending.row);
            let start = pending.row * pending.result.width * 4;
            let length = rows * pending.result.width * 4;
            let submitted = Instant::now();
            self.state.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &pending.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: pending.row as u32,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &pending.result.pixels[start..start + length],
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(pending.result.width as u32 * 4),
                    rows_per_image: Some(rows as u32),
                },
                wgpu::Extent3d {
                    width: pending.result.width as u32,
                    height: rows as u32,
                    depth_or_array_layers: 1,
                },
            );
            performance::elapsed("texture_write_ms", submitted);
            pending.row += rows;
            *remaining -= length;
        }
        if pending.row != pending.result.height {
            return None;
        }
        let pending = self.pending.take().unwrap();
        let view = pending
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let registered = Instant::now();
        let id = self.state.renderer.write().register_native_texture(
            &self.state.device,
            &view,
            wgpu::FilterMode::Linear,
        );
        performance::elapsed("texture_register_ms", registered);
        let image = GpuImage(Arc::new(ImageInner {
            id,
            size: [pending.result.width, pending.result.height],
            texture: pending.texture,
            state: self.state.clone(),
            requested_side: pending.result.max_side,
            allocation: Some(pending.allocation),
        }));
        Some((pending.result, image))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{budget::ByteBudget, thumbnails::ImageKind};
    #[test]
    #[ignore = "requires a DX12 adapter"]
    fn gpu_sliced_upload_readback_and_cancel() {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..Default::default()
        });
        let state = pollster::block_on(RenderState::create(
            &eframe::egui_wgpu::WgpuConfiguration::default(),
            &instance,
            None,
            None,
            1,
            false,
        ))
        .unwrap();
        let service = ThumbnailService::new(Arc::new(|| {}));
        service.begin_preview(Some("test".into()));
        let budget = ByteBudget::new(32 * performance::MIB, 0);
        let make = || ImageResult {
            generation: 0,
            preview_epoch: 1,
            record_id: 1,
            path: "test".into(),
            modified_ns: 0,
            source_key: "test".into(),
            texture_key: "test:preview".into(),
            request_key: "test:preview:4096".into(),
            kind: ImageKind::Preview,
            max_side: 4096,
            pixels: [80, 40, 20, 128].repeat(4096 * 1024),
            width: 4096,
            height: 1024,
            source_width: 4096,
            source_height: 1024,
            error: None,
            failure: None,
            _lease: budget.try_acquire(16 * performance::MIB),
        };
        let mut uploads = Uploads::new(state.clone());
        assert!(uploads.queue(make()).is_ok());
        let mut completed = None;
        let mut frames = 0;
        while completed.is_none() {
            let mut bytes = performance::UPLOAD_BYTES;
            completed = uploads.advance(&service, &mut bytes, Instant::now());
            assert!(bytes <= performance::UPLOAD_BYTES);
            frames += 1;
            assert!(frames < 100);
            state.queue.submit([]);
        }
        assert!(frames >= 4);
        assert_eq!(uploads.used_bytes(), 16 * performance::MIB);
        let (result, texture) = completed.unwrap();
        let buffer = state.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: result.pixels.len() as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = state.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture.0.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4096 * 4),
                    rows_per_image: Some(1024),
                },
            },
            wgpu::Extent3d {
                width: 4096,
                height: 1024,
                depth_or_array_layers: 1,
            },
        );
        state.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = state.device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();
        assert_eq!(&*buffer.slice(..).get_mapped_range(), &result.pixels);
        buffer.unmap();
        drop(result);
        drop(texture);
        assert_eq!(budget.used(), 0);
        assert!(uploads.queue(make()).is_ok());
        assert!(
            uploads
                .advance(
                    &service,
                    &mut performance::UPLOAD_BYTES.clone(),
                    Instant::now()
                )
                .is_none()
        );
        service.begin_preview(None);
        assert!(
            uploads
                .advance(
                    &service,
                    &mut performance::UPLOAD_BYTES.clone(),
                    Instant::now()
                )
                .is_none()
        );
        assert!(!uploads.is_pending());
        assert!(uploads.used_bytes() >= 16 * performance::MIB);
        // Cancellation can occur between write_texture and the frame submission.
        state.queue.submit([]);
        uploads.reclaim();
        let _ = state.device.poll(wgpu::Maintain::Wait);
        let until = Instant::now() + Duration::from_secs(2);
        while budget.used() != 0 && Instant::now() < until {
            std::thread::yield_now();
        }
        assert_eq!(budget.used(), 0);
        assert_eq!(uploads.used_bytes(), 0);
    }
}
