//! Native texture uploads are sliced by bytes; an incomplete texture is never displayed.
use crate::{
    performance,
    thumbnails::{ImageResult, ThumbnailService},
};
use eframe::{egui, egui_wgpu::RenderState};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct GpuImage(Arc<ImageInner>);
struct ImageInner {
    id: egui::TextureId,
    size: [usize; 2],
    texture: wgpu::Texture,
    state: RenderState,
    pub requested_side: u32,
}
impl Drop for ImageInner {
    fn drop(&mut self) {
        self.state.renderer.write().free_texture(&self.id);
        // Drop lets wgpu retain resources referenced by an in-flight submission.
        // Explicit destroy here could invalidate a write queued earlier this frame.
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
}
pub struct Uploads {
    state: RenderState,
    pending: Option<Pending>,
}
impl Uploads {
    pub fn new(state: RenderState) -> Self {
        Self {
            state,
            pending: None,
        }
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
            drop(p);
        }
    }
    pub fn queue(&mut self, result: ImageResult) {
        debug_assert!(self.pending.is_none());
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
        });
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
        let id = self.state.renderer.write().register_native_texture(
            &self.state.device,
            &view,
            wgpu::FilterMode::Linear,
        );
        let image = GpuImage(Arc::new(ImageInner {
            id,
            size: [pending.result.width, pending.result.height],
            texture: pending.texture,
            state: self.state.clone(),
            requested_side: pending.result.max_side,
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
        uploads.queue(make());
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
        uploads.queue(make());
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
        assert_eq!(budget.used(), 0);
        // Cancellation can occur between write_texture and the frame submission.
        state.queue.submit([]);
        let _ = state.device.poll(wgpu::Maintain::Wait);
    }
}
