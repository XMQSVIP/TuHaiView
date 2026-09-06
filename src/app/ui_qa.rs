//! Opt-in full-app layout/interaction QA. Captures DX12 output without an OS window.
use super::*;
use eframe::App;

struct Harness {
    app: PreviewerApp,
    ctx: egui::Context,
    state: eframe::egui_wgpu::RenderState,
    size: egui::Vec2,
    scale: f32,
    text_rects: HashMap<String, egui::Rect>,
}

impl Harness {
    fn frame(&mut self, events: Vec<egui::Event>, capture: Option<&std::path::Path>) {
        self.ctx.set_pixels_per_point(self.scale);
        let output = self.ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size)),
                events,
                ..Default::default()
            },
            |ctx| self.app.update(ctx, &mut eframe::Frame::_new_kittest()),
        );
        self.text_rects.clear();
        for shape in &output.shapes {
            if let egui::Shape::Text(text) = &shape.shape {
                self.text_rects
                    .insert(text.galley.job.text.clone(), text.visual_bounding_rect());
            }
        }
        let mut renderer = self.state.renderer.write();
        for (id, delta) in &output.textures_delta.set {
            renderer.update_texture(&self.state.device, &self.state.queue, *id, delta);
        }
        if let Some(path) = capture {
            let jobs = self.ctx.tessellate(output.shapes, output.pixels_per_point);
            let width = (self.size.x * output.pixels_per_point).round() as u32;
            let height = (self.size.y * output.pixels_per_point).round() as u32;
            let device = &self.state.device;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("UI QA capture"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.state.target_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let stride = (width * 4).div_ceil(256) * 256;
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: (stride * height) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = device.create_command_encoder(&Default::default());
            let screen = eframe::egui_wgpu::ScreenDescriptor {
                size_in_pixels: [width, height],
                pixels_per_point: output.pixels_per_point,
            };
            let commands =
                renderer.update_buffers(device, &self.state.queue, &mut encoder, &jobs, &screen);
            {
                let view = texture.create_view(&Default::default());
                let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                renderer.render(&mut pass.forget_lifetime(), &jobs, &screen);
            }
            encoder.copy_texture_to_buffer(
                texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(stride),
                        rows_per_image: Some(height),
                    },
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
            self.state
                .queue
                .submit(commands.into_iter().chain([encoder.finish()]));
            let (tx, rx) = std::sync::mpsc::channel();
            buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
            let _ = device.poll(wgpu::Maintain::Wait);
            rx.recv().unwrap().unwrap();
            let data = buffer.slice(..).get_mapped_range();
            let mut bytes = Vec::with_capacity((width * height * 4) as usize);
            for row in data.chunks(stride as usize) {
                bytes.extend_from_slice(&row[..width as usize * 4]);
            }
            if matches!(
                self.state.target_format,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
            ) {
                for pixel in bytes.chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                }
            }
            image::save_buffer(path, &bytes, width, height, image::ColorType::Rgba8).unwrap();
        }
        for id in &output.textures_delta.free {
            renderer.free_texture(id);
        }
    }

    fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.frame(vec![], None);
            if !self.app.scanning && !self.app.sorting && !self.app.search.pending() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "UI failed to settle: {}",
                self.app.status
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn search(&mut self, text: &str) {
        self.frame(vec![key(egui::Key::F, egui::Modifiers::CTRL)], None);
        self.frame(vec![egui::Event::Text(text.into())], None);
    }

    fn click_text(&mut self, text: &str) {
        for _ in 0..4 {
            self.frame(vec![], None);
        }
        let pos = self
            .text_rects
            .get(text)
            .unwrap_or_else(|| panic!("missing widget: {text}"))
            .center();
        self.click_at(pos);
    }

    fn click_at(&mut self, pos: egui::Pos2) {
        self.frame(vec![egui::Event::PointerMoved(pos)], None);
        for pressed in [true, false] {
            self.frame(
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
                None,
            );
        }
        self.frame(vec![egui::Event::PointerGone], None);
    }
}

fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

#[test]
#[ignore = "DX12 full-app captures and 50k UI benchmark; copy EXE into a new tuhai-ui-qa-* directory"]
fn layout_interaction_and_search_50k() {
    assert!(!cfg!(debug_assertions), "use Release");
    let root = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    assert!(
        root.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("tuhai-ui-qa-")
    );
    let captures = root.join("captures");
    fs::create_dir_all(&captures).unwrap();
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        ..Default::default()
    });
    let state = pollster::block_on(eframe::egui_wgpu::RenderState::create(
        &Default::default(),
        &instance,
        None,
        None,
        1,
        false,
    ))
    .unwrap();
    let ctx = egui::Context::default();
    ctx.set_visuals(egui::Visuals::light());
    let mut cc = eframe::CreationContext::_new_kittest(ctx.clone());
    cc.wgpu_render_state = Some(state.clone());
    let mut h = Harness {
        app: PreviewerApp::new(&cc).unwrap(),
        ctx,
        state,
        size: egui::vec2(1280.0, 820.0),
        scale: 1.0,
        text_rects: HashMap::new(),
    };
    assert!(h.app.root.is_none(), "run without TUHAI_PERF_ROOT");
    // Exercise real toolbar hit targets, including opening a dropdown without
    // changing its value and switching away from a pending confirmation.
    h.click_text("关于");
    assert!(h.app.show_about);
    h.click_text("刷新 (F5)");
    assert!(
        h.app.show_about,
        "disabled entries must not dismiss the current window"
    );
    h.click_text("修改时间↓");
    assert!(!h.app.show_about);
    h.frame(vec![key(egui::Key::Escape, egui::Modifiers::NONE)], None);
    h.click_text("关于");
    h.click_text("缩略图缓存 1 GiB");
    assert!(!h.app.show_about);
    h.frame(vec![key(egui::Key::Escape, egui::Modifiers::NONE)], None);
    h.click_text("关于");
    h.click_text("清理缓存和数据库");
    assert!(!h.app.show_about && matches!(h.app.pending, Some(PendingDialog::ClearStorage)));
    h.click_text("关于");
    assert!(h.app.show_about && h.app.pending.is_none());
    h.app.close_auxiliary_windows();
    let background_root = root.join("background-empty-scan");
    fs::create_dir_all(&background_root).unwrap();
    h.app.auxiliary_target = Some(AuxiliaryWindow::EmptyFolders);
    h.app.empty_folder_generation = h.app.empty_folder_service.scan(background_root);
    h.app.empty_folder_scanning = true;
    h.app.open_auxiliary_window(AuxiliaryWindow::About);
    h.click_text("修改时间↓");
    let deadline = Instant::now() + Duration::from_secs(10);
    while h.app.empty_folder_scanning {
        h.frame(vec![], None);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!h.app.show_about && !h.app.show_empty && !h.app.show_duplicates);
    h.frame(vec![key(egui::Key::Escape, egui::Modifiers::NONE)], None);
    println!(
        "menu switching: About -> sort/cache/clear, clear -> About, disabled entry, late scan completion passed"
    );
    for (width, height) in [(900, 600), (1280, 820), (1920, 1080)] {
        for scale in [1.0, 1.25, 1.5] {
            h.size = egui::vec2(width as f32, height as f32);
            h.scale = scale;
            for _ in 0..4 {
                h.frame(vec![], None);
            }
            h.frame(
                vec![],
                Some(&captures.join(format!("welcome-{width}-{}.png", (scale * 100.0) as u32))),
            );
        }
    }
    let pictures = root
        .join("图片测试目录_用于验证完整路径在工具栏中的截断显示")
        .join("子文件夹_保留原图与现有操作习惯");
    fs::create_dir_all(&pictures).unwrap();
    for i in 0..37 {
        let name = if i == 0 {
            "旅行照片_包含中文和很长名称_原始图片.JPG".into()
        } else {
            format!("image-{i:02}.png")
        };
        let picture = image::RgbImage::from_fn(160, 100 + i, |x, y| {
            image::Rgb([(x + i * 4) as u8, (y * 2) as u8, (80 + i * 3) as u8])
        });
        picture.save(pictures.join(name)).unwrap();
    }
    h.app.open_root(pictures);
    h.settle();
    for _ in 0..50 {
        h.frame(vec![], None);
        std::thread::sleep(Duration::from_millis(10));
    }
    h.app.selected.extend(
        h.app
            .display_indices
            .iter()
            .take(3)
            .map(|i| h.app.records[*i].id),
    );
    // Check actual checkbox clicks do not change this row or any following row.
    for _ in 0..4 {
        h.frame(vec![], None);
    }
    let first = h.app.display_record(0).unwrap().clone();
    let positions: HashMap<_, _> = h
        .text_rects
        .iter()
        .filter(|(name, _)| name.starts_with("image-"))
        .map(|(n, r)| (n.clone(), *r))
        .collect();
    let label = h.text_rects[&first.file_name];
    let checkbox = egui::pos2(label.center().x, label.top() - 12.0);
    for selected in [false, true] {
        h.click_at(checkbox);
        for _ in 0..4 {
            h.frame(vec![], None);
        }
        assert_eq!(
            h.app.selected.contains(&first.id),
            selected,
            "checkbox hit target"
        );
        for (name, rect) in &positions {
            assert_eq!(h.text_rects.get(name), Some(rect), "selection moved {name}");
        }
    }
    println!("checkbox layout: select/unselect preserves visible filename positions");
    h.app.status = "扫描完成：已收录 50000 张图片，复用 49999 张，新增 1 张，更新 0 张，错误 0，长状态显示测试".into();
    for (width, height) in [(900, 600), (1280, 820), (1920, 1080)] {
        for scale in [1.0, 1.25, 1.5] {
            h.size = egui::vec2(width as f32, height as f32);
            h.scale = scale;
            for _ in 0..4 {
                h.frame(vec![], None);
            }
            h.frame(
                vec![],
                Some(&captures.join(format!("selected-{width}-{}.png", (scale * 100.0) as u32))),
            );
        }
    }
    h.size = egui::vec2(1280.0, 820.0);
    h.scale = 1.0;
    for _ in 0..4 {
        h.frame(vec![], None);
    }
    h.app.selected.clear();
    h.frame(vec![], Some(&captures.join("grid.png")));
    h.search("image-1");
    h.frame(vec![], Some(&captures.join("search-pending.png")));
    h.settle();
    assert_eq!(h.app.display_indices.len(), 10);
    h.frame(vec![key(egui::Key::Escape, egui::Modifiers::NONE)], None);
    h.settle();
    h.frame(vec![key(egui::Key::Escape, egui::Modifiers::NONE)], None);
    h.frame(vec![key(egui::Key::A, egui::Modifiers::CTRL)], None);
    assert_eq!(h.app.selected.len(), 37);
    h.search("image-1");
    h.settle();
    assert_eq!(h.app.selected.len(), 10);
    let first = h.app.display_record(0).unwrap().id;
    h.app.open_preview(0);
    for _ in 0..4 {
        h.frame(vec![], None);
    }
    h.frame(vec![], Some(&captures.join("preview-light.png")));
    h.frame(
        vec![key(egui::Key::ArrowRight, egui::Modifiers::NONE)],
        None,
    );
    assert_eq!(h.app.preview, Some(1));
    h.frame(vec![key(egui::Key::ArrowLeft, egui::Modifiers::NONE)], None);
    assert_eq!(h.app.preview, Some(0));
    assert_eq!(h.app.display_record(0).unwrap().id, first);
    h.frame(vec![key(egui::Key::Escape, egui::Modifiers::NONE)], None);
    assert!(h.app.preview.is_none());
    assert_eq!(h.app.display_indices.len(), 10);
    let copied = root.join("copied-selection");
    fs::create_dir_all(&copied).unwrap();
    h.app.submit_selected(
        FileAction::Copy,
        Some(copied.clone()),
        ConflictPolicy::AutoRename,
        HashMap::new(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while h.app.file_operation_running {
        h.frame(vec![], None);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fs::read_dir(copied).unwrap().count(), 10);
    h.search("no-match");
    h.settle();
    assert!(h.app.selected.is_empty());
    h.frame(vec![], Some(&captures.join("no-match.png")));
    h.app.displayed_hidden_duplicates = 2;
    h.frame(vec![], Some(&captures.join("no-match-hidden.png")));
    h.app.search.clear();
    h.app.request_sort();
    h.settle();
    assert!(h.app.selected.is_empty());
    let empty = root.join("empty");
    fs::create_dir_all(&empty).unwrap();
    h.app.open_root(empty);
    h.settle();
    h.frame(vec![], Some(&captures.join("empty.png")));
    // Synthetic membership, shared real temporary image: no 50k file copies or original files touched.
    let sample = image::RgbImage::new(8, 8);
    let sample_path = root.join("sample.png");
    sample.save(&sample_path).unwrap();
    verify_scan_order(&mut h, &sample_path, &captures);
    let records = (0..50_000)
        .map(|i| {
            Arc::new(ImageRecord {
                id: i,
                path: sample_path.clone(),
                relative_path: format!("旅行 IMG {i:05}.JPG"),
                file_name: format!("旅行 IMG {i:05}.JPG"),
                size: 100,
                modified_ns: 0,
                width: Some(8),
                height: Some(8),
                format: "jpg".into(),
                thumbnail_key: "qa-shared".into(),
                content_hash: None,
            })
        })
        .collect();
    h.app.pending_snapshot = Some(Arc::new(CatalogSnapshot::new(
        h.app.generation,
        h.app.data_revision + 1,
        records,
    )));
    h.app.sort = SortMode::NameNatural;
    h.app.request_sort();
    h.settle();
    for (label, text, count) in [
        ("many", "旅行", 50_000),
        ("few", "49999", 1),
        ("none", "missing", 0),
    ] {
        let mut times = Vec::new();
        for _ in 0..20 {
            h.frame(vec![key(egui::Key::F, egui::Modifiers::CTRL)], None);
            let started = Instant::now();
            h.frame(vec![egui::Event::Text(text.into())], None);
            loop {
                h.frame(vec![], None);
                if !h.app.search.pending() && !h.app.sorting {
                    break;
                }
                assert!(started.elapsed() < Duration::from_secs(5));
                std::thread::sleep(Duration::from_millis(16));
            }
            assert_eq!(h.app.display_indices.len(), count);
            times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        println!(
            "ui_apply_50k {label}: n=20 p50={:.2} p95={:.2} max={:.2} ms (16ms test frame pump; no native presentation)",
            times[9], times[18], times[19]
        );
        assert!(times[18] <= 300.0);
    }
    h.app.search.clear();
    h.app.request_sort();
    h.settle();
    let mut times = Vec::new();
    for i in 0..240 {
        h.app.pending_grid_scroll_offset = Some(i as f32 * 100.0);
        let started = Instant::now();
        h.frame(vec![], None);
        if i >= 40 {
            times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
    }
    times.sort_by(f64::total_cmp);
    println!(
        "ui_scroll_50k: n=200 update+texture_delta p50={:.2} p95={:.2} max={:.2} ms (no native presentation)",
        times[99], times[189], times[199]
    );
    println!("DX12 captures: {}", captures.display());
}

fn verify_scan_order(h: &mut Harness, sample: &std::path::Path, captures: &std::path::Path) {
    for number in 1..=36 {
        fs::copy(sample, sample.with_file_name(format!("batch-{number}.png"))).unwrap();
    }
    h.app.sort = SortMode::ModifiedDesc;
    h.app.scanning = true;
    h.app.status = "扫描分批显示回归".into();
    let mut ids = Vec::new();
    let mut label_positions = HashMap::new();
    for count in [12, 24, 36] {
        let records = (0..count)
            .rev()
            .map(|id| {
                Arc::new(ImageRecord {
                    id: 100_000 + id,
                    path: sample.with_file_name(format!("batch-{}.png", 36 - id)),
                    relative_path: format!("batch-{}.png", 36 - id),
                    file_name: format!("batch-{}.png", 36 - id),
                    size: 100,
                    modified_ns: id,
                    width: Some(8),
                    height: Some(8),
                    format: "png".into(),
                    thumbnail_key: "qa-scan-shared".into(),
                    content_hash: None,
                })
            })
            .collect();
        h.app.pending_snapshot = Some(Arc::new(CatalogSnapshot::new(
            h.app.generation,
            h.app.data_revision + 1,
            records,
        )));
        h.app.request_catalog_sort();
        let deadline = Instant::now() + Duration::from_secs(10);
        while h.app.sorting {
            h.frame(vec![], None);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        for _ in 0..4 {
            h.frame(vec![], None);
        }
        let next: Vec<_> = h
            .app
            .display_indices
            .iter()
            .map(|i| h.app.records[*i].id)
            .collect();
        assert!(
            next.starts_with(&ids),
            "app scan publication moved existing pictures"
        );
        for (label, rect) in &label_positions {
            assert_eq!(h.text_rects.get(label), Some(rect), "scan moved {label}");
        }
        ids = next;
        if count == 12 {
            label_positions = h
                .text_rects
                .iter()
                .filter(|(name, _)| name.starts_with("batch-"))
                .map(|(name, rect)| (name.clone(), *rect))
                .collect();
            assert!(!label_positions.is_empty());
        }
        h.frame(
            vec![],
            Some(&captures.join(format!("scan-batch-{count}.png"))),
        );
    }
    let preview_id = h.app.display_record(0).unwrap().id;
    h.app.selected.insert(preview_id);
    h.app.open_preview(0);
    h.app.scanning = false;
    h.app.request_sort();
    h.settle();
    let sorted: Vec<_> = h
        .app
        .display_indices
        .iter()
        .map(|i| h.app.records[*i].id)
        .collect();
    assert!(sorted.windows(2).all(|pair| pair[0] > pair[1]));
    assert_eq!(
        h.app.display_record(h.app.preview.unwrap()).unwrap().id,
        preview_id
    );
    assert!(h.app.selected.contains(&preview_id));
    h.app.close_preview();
    h.app.selected.clear();
    // An explicit search during a scan must use the selected ordering immediately.
    h.app.scanning = true;
    h.search("batch-1");
    let deadline = Instant::now() + Duration::from_secs(10);
    while h.app.search.pending() || h.app.sorting {
        h.frame(vec![], None);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(h.app.display_indices.len(), 11);
    let filtered: Vec<_> = h
        .app
        .display_indices
        .iter()
        .map(|i| h.app.records[*i].id)
        .collect();
    assert!(filtered.windows(2).all(|pair| pair[0] > pair[1]));
    h.app.scanning = false;
    h.app.search.clear();
    h.app.request_sort();
    h.settle();
    println!(
        "scan stability: 3 batches preserve published IDs and filename rectangles; final order, preview ID, selection and search during scan passed"
    );
}
