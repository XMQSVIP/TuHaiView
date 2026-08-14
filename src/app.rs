use crate::{
    catalog::{CatalogEvent, CatalogService, MetadataUpdate},
    empty_folders::{EmptyFolderEvent, EmptyFolderService},
    file_ops::{self, FileOperationService},
    models::{
        ConflictPolicy, EmptyFolderCandidate, FileAction, FileOperationRequest, ImageRecord,
        SortMode,
    },
    sorting::SortService,
    thumbnails::{ImageKind, ThumbnailPriority, ThumbnailService, texture_key},
};
use eframe::egui::{self, ColorImage, TextureHandle};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

enum PendingDialog {
    Delete { permanent: bool },
    Transfer(TransferDialog),
    DeleteEmpty { permanent: bool },
    ClearStorage,
}

struct TransferDialog {
    action: FileAction,
    destination: PathBuf,
    conflicts: Vec<PathBuf>,
    conflict_index: usize,
    decisions: HashMap<PathBuf, ConflictPolicy>,
    apply_to_remaining: bool,
}

pub struct PreviewerApp {
    catalog: CatalogService,
    empty_folder_service: EmptyFolderService,
    thumbnails: ThumbnailService,
    file_ops: FileOperationService,
    sort_service: SortService,
    root: Option<PathBuf>,
    generation: u64,
    records: Vec<ImageRecord>,
    record_positions: HashMap<PathBuf, usize>,
    textures: HashMap<String, TextureHandle>,
    texture_last_used: HashMap<String, u64>,
    texture_clock: u64,
    texture_bytes: usize,
    failed_images: HashSet<String>,
    selected: HashSet<i64>,
    selection_anchor: Option<usize>,
    sort: SortMode,
    previous_sort: SortMode,
    data_revision: u64,
    sorting: bool,
    thumb_size: u32,
    status: String,
    scanning: bool,
    file_operation_running: bool,
    preview: Option<usize>,
    preview_origin: Option<usize>,
    grid_scroll_offset: f32,
    preview_return_offset: f32,
    pending_grid_scroll_offset: Option<f32>,
    pending_grid_focus: Option<usize>,
    prefetch_rows: Option<(usize, usize)>,
    zoom: f32,
    rotation_quarters: u8,
    fit_preview: bool,
    show_about: bool,
    show_empty: bool,
    empty_folders: Vec<EmptyFolderCandidate>,
    empty_folder_generation: u64,
    empty_folder_scanning: bool,
    empty_folder_visited: usize,
    empty_folder_found: usize,
    empty_folder_errors: usize,
    pending: Option<PendingDialog>,
    conflict_policy: ConflictPolicy,
    rescan_due: Option<Instant>,
}

impl PreviewerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> anyhow::Result<Self> {
        configure_chinese_font(&cc.egui_ctx);
        let repaint_context = cc.egui_ctx.clone();
        let wakeup: Arc<dyn Fn() + Send + Sync> =
            Arc::new(move || repaint_context.request_repaint());
        Ok(Self {
            catalog: CatalogService::new(wakeup.clone())?,
            empty_folder_service: EmptyFolderService::new(wakeup.clone()),
            thumbnails: ThumbnailService::new(wakeup.clone()),
            file_ops: FileOperationService::new(wakeup.clone()),
            sort_service: SortService::new(wakeup),
            root: None,
            generation: 0,
            records: Vec::new(),
            record_positions: HashMap::new(),
            textures: HashMap::new(),
            texture_last_used: HashMap::new(),
            texture_clock: 0,
            texture_bytes: 0,
            failed_images: HashSet::new(),
            selected: HashSet::new(),
            selection_anchor: None,
            sort: SortMode::ModifiedDesc,
            previous_sort: SortMode::ModifiedDesc,
            data_revision: 0,
            sorting: false,
            thumb_size: 160,
            status: "请选择一个文件夹".into(),
            scanning: false,
            file_operation_running: false,
            preview: None,
            preview_origin: None,
            grid_scroll_offset: 0.0,
            preview_return_offset: 0.0,
            pending_grid_scroll_offset: None,
            pending_grid_focus: None,
            prefetch_rows: None,
            zoom: 1.0,
            rotation_quarters: 0,
            fit_preview: true,
            show_about: false,
            show_empty: false,
            empty_folders: Vec::new(),
            empty_folder_generation: 0,
            empty_folder_scanning: false,
            empty_folder_visited: 0,
            empty_folder_found: 0,
            empty_folder_errors: 0,
            pending: None,
            conflict_policy: ConflictPolicy::Ask,
            rescan_due: None,
        })
    }

    fn choose_root(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            self.open_root(path);
        }
    }

    fn open_root(&mut self, path: PathBuf) {
        self.empty_folder_generation = self.empty_folder_service.cancel();
        self.empty_folder_scanning = false;
        self.show_empty = false;
        self.empty_folders.clear();
        self.root = Some(path.clone());
        self.records.clear();
        self.record_positions.clear();
        self.data_revision = self.data_revision.wrapping_add(1);
        self.sorting = false;
        self.selected.clear();
        self.selection_anchor = None;
        self.textures.clear();
        self.texture_last_used.clear();
        self.texture_clock = 0;
        self.texture_bytes = 0;
        self.failed_images.clear();
        self.preview = None;
        self.preview_origin = None;
        self.grid_scroll_offset = 0.0;
        self.pending_grid_scroll_offset = Some(0.0);
        self.pending_grid_focus = None;
        self.prefetch_rows = None;
        self.status = "正在载入缓存并扫描…".into();
        self.scanning = true;
        self.catalog.scan(path, self.sort);
    }

    fn refresh(&mut self) {
        if let Some(root) = self.root.clone() {
            self.scanning = true;
            self.status = "正在增量校验…".into();
            self.catalog.scan(root, self.sort);
        }
    }

    fn process_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.catalog.rx.try_recv() {
            match event {
                CatalogEvent::Started { generation } => {
                    self.generation = generation;
                    self.thumbnails.set_generation(generation);
                }
                CatalogEvent::Snapshot {
                    generation,
                    records,
                } if generation == self.generation => {
                    let preview_ids = self.preview_record_ids();
                    self.records = records;
                    self.data_revision = self.data_revision.wrapping_add(1);
                    self.rebuild_record_positions();
                    self.restore_preview_by_ids(preview_ids);
                    self.status = format!("已载入 {} 条缓存，正在增量校验…", self.records.len());
                }
                CatalogEvent::UpsertBatch {
                    generation,
                    records,
                } if generation == self.generation => {
                    let mut changed = false;
                    for record in records {
                        changed = true;
                        if let Some(index) = self.record_positions.get(&record.path).copied() {
                            self.records[index] = record;
                        } else {
                            let index = self.records.len();
                            self.record_positions.insert(record.path.clone(), index);
                            self.records.push(record);
                        }
                    }
                    if changed {
                        self.data_revision = self.data_revision.wrapping_add(1);
                    }
                }
                CatalogEvent::RemoveBatch { generation, ids } if generation == self.generation => {
                    let preview_ids = self.preview_record_ids();
                    let removed = ids.into_iter().collect::<HashSet<_>>();
                    self.records.retain(|record| !removed.contains(&record.id));
                    self.selected.retain(|id| !removed.contains(id));
                    self.data_revision = self.data_revision.wrapping_add(1);
                    self.rebuild_record_positions();
                    self.restore_preview_by_ids(preview_ids);
                }
                CatalogEvent::Progress {
                    generation,
                    visited_files,
                    supported_images,
                    reused,
                    inserted,
                    updated,
                } if generation == self.generation => {
                    self.status = format!(
                        "正在扫描：文件 {visited_files}，图片 {supported_images}，复用 {reused}，新增 {inserted}，更新 {updated}"
                    );
                }
                CatalogEvent::Finished {
                    generation,
                    total,
                    stats,
                } if generation == self.generation => {
                    self.scanning = false;
                    self.status = format!(
                        "已找到 {total} 张：复用 {}，新增 {}，更新 {}，删除 {}，遍历错误 {}，耗时 {:.2}s（数据库 {}ms）",
                        stats.reused,
                        stats.inserted,
                        stats.updated,
                        stats.removed,
                        stats.traversal_errors,
                        stats.elapsed_ms as f64 / 1000.0,
                        stats.db_write_ms,
                    );
                    self.request_sort();
                }
                CatalogEvent::Error {
                    generation,
                    message,
                } if generation == self.generation => {
                    self.scanning = false;
                    self.status = format!("扫描失败：{message}");
                }
                CatalogEvent::Changed { generation } if generation == self.generation => {
                    self.rescan_due = Some(Instant::now() + Duration::from_millis(700));
                }
                _ => {}
            }
        }

        while let Ok(event) = self.empty_folder_service.rx.try_recv() {
            match event {
                EmptyFolderEvent::Progress {
                    generation,
                    visited_directories,
                    found,
                    errors,
                } if generation == self.empty_folder_generation => {
                    self.empty_folder_visited = visited_directories;
                    self.empty_folder_found = found;
                    self.empty_folder_errors = errors;
                }
                EmptyFolderEvent::Finished {
                    generation,
                    folders,
                    visited_directories,
                    errors,
                    elapsed_ms,
                } if generation == self.empty_folder_generation => {
                    self.empty_folder_scanning = false;
                    self.empty_folder_visited = visited_directories;
                    self.empty_folder_found = folders.len();
                    self.empty_folder_errors = errors;
                    self.empty_folders = folders;
                    self.show_empty = true;
                    self.status = format!(
                        "空文件夹扫描完成：找到 {} 个，遍历 {} 个目录，错误 {}，耗时 {:.2}s",
                        self.empty_folder_found,
                        self.empty_folder_visited,
                        errors,
                        elapsed_ms as f64 / 1000.0,
                    );
                }
                EmptyFolderEvent::Error {
                    generation,
                    message,
                } if generation == self.empty_folder_generation => {
                    self.empty_folder_scanning = false;
                    self.status = format!("扫描空文件夹失败：{message}");
                }
                _ => {}
            }
        }

        while let Ok(result) = self.thumbnails.rx.try_recv() {
            if result.generation != self.generation {
                continue;
            }
            if let Some(error) = result.error {
                self.failed_images.insert(result.texture_key);
                self.status = format!("图片解码失败：{error}");
                continue;
            }
            let metadata_changed = self
                .record_positions
                .get(&result.path)
                .copied()
                .and_then(|index| self.records.get_mut(index))
                .is_some_and(|record| {
                    if record.id != result.record_id
                        || record.modified_ns != result.modified_ns
                        || result.source_width == 0
                        || result.source_height == 0
                    {
                        return false;
                    }
                    let changed = record.width != Some(result.source_width)
                        || record.height != Some(result.source_height);
                    if changed {
                        record.width = Some(result.source_width);
                        record.height = Some(result.source_height);
                    }
                    changed
                });
            if metadata_changed {
                self.catalog.queue_metadata_update(MetadataUpdate {
                    id: result.record_id,
                    path: result.path.clone(),
                    modified_ns: result.modified_ns,
                    width: result.source_width,
                    height: result.source_height,
                });
            }
            let image =
                ColorImage::from_rgba_unmultiplied([result.width, result.height], &result.pixels);
            let bytes = result.pixels.len();
            let texture = ctx.load_texture(
                result.texture_key.clone(),
                image,
                egui::TextureOptions::LINEAR,
            );
            self.insert_texture(result.texture_key, texture, bytes);
        }

        while let Ok((action, report)) = self.file_ops.rx.try_recv() {
            self.file_operation_running = false;
            let success_ids: HashSet<_> = self
                .records
                .iter()
                .filter(|record| report.succeeded.iter().any(|path| path == &record.path))
                .map(|record| record.id)
                .collect();
            if matches!(
                action,
                FileAction::Move | FileAction::RecycleDelete | FileAction::PermanentDelete
            ) {
                let preview_ids = self.preview_record_ids();
                self.records
                    .retain(|record| !success_ids.contains(&record.id));
                self.selected.retain(|id| !success_ids.contains(id));
                self.data_revision = self.data_revision.wrapping_add(1);
                self.rebuild_record_positions();
                self.restore_preview_by_ids(preview_ids);
            }
            self.status = format!(
                "操作完成：成功 {}，跳过 {}，失败 {}{}",
                report.succeeded.len(),
                report.skipped.len(),
                report.failed.len(),
                if report.cancelled {
                    "，用户取消了部分操作"
                } else {
                    ""
                }
            );
            if let Some(root) = self.root.clone() {
                self.rescan_due = Some(Instant::now() + Duration::from_millis(800));
                if report.failed.is_empty() && action == FileAction::Copy && root.exists() {
                    ctx.request_repaint();
                }
            }
        }

        while let Ok(result) = self.sort_service.rx.try_recv() {
            if result.generation == self.generation
                && result.revision == self.data_revision
                && result.mode == self.sort
            {
                let preview_ids = self.preview_record_ids();
                self.records = result.records;
                self.rebuild_record_positions();
                self.restore_preview_by_ids(preview_ids);
                self.selection_anchor = None;
                self.sorting = false;
            }
        }

        if self.rescan_due.is_some_and(|due| Instant::now() >= due)
            && !self.scanning
            && !self.file_operation_running
        {
            self.rescan_due = None;
            self.refresh();
        }
    }

    fn rebuild_record_positions(&mut self) {
        self.record_positions.clear();
        self.record_positions.reserve(self.records.len());
        for (index, record) in self.records.iter().enumerate() {
            self.record_positions.insert(record.path.clone(), index);
        }
    }

    fn preview_record_ids(&self) -> (Option<i64>, Option<i64>) {
        let current = self
            .preview
            .and_then(|index| self.records.get(index))
            .map(|record| record.id);
        let origin = self
            .preview_origin
            .and_then(|index| self.records.get(index))
            .map(|record| record.id);
        (current, origin)
    }

    fn restore_preview_by_ids(&mut self, (current, origin): (Option<i64>, Option<i64>)) {
        self.preview =
            current.and_then(|id| self.records.iter().position(|record| record.id == id));
        self.preview_origin =
            origin.and_then(|id| self.records.iter().position(|record| record.id == id));
        if self.preview.is_none() {
            self.preview_origin = None;
        }
    }

    fn request_sort(&mut self) {
        self.sort_service.submit(
            self.generation,
            self.data_revision,
            self.sort,
            self.records.clone(),
        );
        self.previous_sort = self.sort;
        self.sorting = true;
    }

    fn insert_texture(&mut self, key: String, texture: TextureHandle, bytes: usize) {
        if let Some(old) = self.textures.insert(key.clone(), texture) {
            self.texture_bytes = self
                .texture_bytes
                .saturating_sub(old.size()[0] * old.size()[1] * 4);
        }
        self.texture_bytes += bytes;
        self.texture_clock = self.texture_clock.wrapping_add(1);
        self.texture_last_used
            .insert(key.clone(), self.texture_clock);
        const LIMIT: usize = 256 * 1024 * 1024;
        while self.texture_bytes > LIMIT {
            let Some(oldest) = self
                .texture_last_used
                .iter()
                .min_by_key(|(_, last_used)| **last_used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.texture_last_used.remove(&oldest);
            if let Some(texture) = self.textures.remove(&oldest) {
                self.texture_bytes = self
                    .texture_bytes
                    .saturating_sub(texture.size()[0] * texture.size()[1] * 4);
            }
        }
    }

    fn touch_texture(&mut self, key: &str) {
        if self.textures.contains_key(key) {
            self.texture_clock = self.texture_clock.wrapping_add(1);
            self.texture_last_used
                .insert(key.to_owned(), self.texture_clock);
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("选择文件夹").clicked() {
                self.choose_root();
            }
            if ui
                .add_enabled(
                    self.root.is_some() && !self.scanning,
                    egui::Button::new("刷新 (F5)"),
                )
                .clicked()
            {
                self.refresh();
            }
            if ui
                .add_enabled(
                    self.root.is_some() && !self.empty_folder_scanning,
                    egui::Button::new("扫描空文件夹"),
                )
                .clicked()
            {
                if let Some(root) = self.root.clone() {
                    self.empty_folder_generation = self.empty_folder_service.scan(root);
                    self.empty_folder_scanning = true;
                    self.empty_folder_visited = 0;
                    self.empty_folder_found = 0;
                    self.empty_folder_errors = 0;
                    self.status = "正在后台扫描空文件夹…".into();
                }
            }
            if self.empty_folder_scanning && ui.button("停止空目录扫描").clicked() {
                self.empty_folder_generation = self.empty_folder_service.cancel();
                self.empty_folder_scanning = false;
                self.status = format!(
                    "已停止空文件夹扫描：遍历 {} 个目录，暂时找到 {} 个",
                    self.empty_folder_visited, self.empty_folder_found
                );
            }
            if ui
                .add_enabled(
                    !self.scanning && !self.file_operation_running,
                    egui::Button::new("清理缓存和数据库"),
                )
                .clicked()
            {
                self.pending = Some(PendingDialog::ClearStorage);
            }
            if ui.button("关于").clicked() {
                self.show_about = true;
            }
            ui.separator();
            ui.label(
                self.root
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            );
            ui.separator();
            ui.label(&self.status);
            if self.empty_folder_scanning {
                ui.spinner();
                ui.label(format!(
                    "空目录：已遍历 {}，找到 {}，错误 {}",
                    self.empty_folder_visited, self.empty_folder_found, self.empty_folder_errors
                ));
            }
            if self.scanning {
                ui.spinner();
            }
            if self.sorting {
                ui.spinner();
                ui.label("正在排序…");
            }
            ui.separator();
            egui::ComboBox::from_id_salt("sort")
                .selected_text(sort_label(self.sort))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.sort, SortMode::ModifiedDesc, "修改时间↓");
                    ui.selectable_value(&mut self.sort, SortMode::NameNatural, "文件名自然排序");
                    ui.selectable_value(&mut self.sort, SortMode::SizeDesc, "文件大小↓");
                    ui.selectable_value(&mut self.sort, SortMode::Path, "文件夹路径");
                });
            ui.add(egui::Slider::new(&mut self.thumb_size, 96..=240).text("缩略图"));
        });
        if self.sort != self.previous_sort {
            self.request_sort();
        }
    }

    fn batch_bar(&mut self, ui: &mut egui::Ui) {
        if self.selected.is_empty() {
            return;
        }
        let total_size: u64 = self
            .records
            .iter()
            .filter(|record| self.selected.contains(&record.id))
            .map(|record| record.size)
            .sum();
        ui.horizontal_wrapped(|ui| {
            ui.label(format!(
                "已选 {} 张，共 {}",
                self.selected.len(),
                format_bytes(total_size)
            ));
            if ui
                .add_enabled(!self.file_operation_running, egui::Button::new("复制到…"))
                .clicked()
            {
                self.prepare_transfer(FileAction::Copy);
            }
            if ui
                .add_enabled(!self.file_operation_running, egui::Button::new("剪切到…"))
                .clicked()
            {
                self.prepare_transfer(FileAction::Move);
            }
            if ui
                .add_enabled(
                    !self.file_operation_running,
                    egui::Button::new("移入回收站 (Delete)"),
                )
                .clicked()
            {
                self.pending = Some(PendingDialog::Delete { permanent: false });
            }
            if ui
                .add_enabled(
                    !self.file_operation_running,
                    egui::Button::new("永久删除 (Shift+Delete)"),
                )
                .clicked()
            {
                self.pending = Some(PendingDialog::Delete { permanent: true });
            }
            if ui.button("清除选择 (Esc)").clicked() {
                self.selected.clear();
            }
        });
    }

    fn prepare_transfer(&mut self, action: FileAction) {
        let Some(destination) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        let selected = self.selected_records();
        let mut counts = HashMap::<String, usize>::new();
        for record in &selected {
            if let Some(name) = record.path.file_name() {
                *counts
                    .entry(name.to_string_lossy().to_lowercase())
                    .or_default() += 1;
            }
        }
        let conflicts = selected
            .iter()
            .filter(|record| {
                record.path.file_name().is_some_and(|name| {
                    destination.join(name).exists()
                        || counts
                            .get(&name.to_string_lossy().to_lowercase())
                            .copied()
                            .unwrap_or(0)
                            > 1
                })
            })
            .map(|record| record.path.clone())
            .collect::<Vec<_>>();
        self.conflict_policy = ConflictPolicy::AutoRename;
        self.pending = Some(PendingDialog::Transfer(TransferDialog {
            action,
            destination,
            conflicts,
            conflict_index: 0,
            decisions: HashMap::new(),
            apply_to_remaining: false,
        }));
    }

    fn selected_records(&self) -> Vec<ImageRecord> {
        self.records
            .iter()
            .filter(|record| self.selected.contains(&record.id))
            .cloned()
            .collect()
    }

    fn submit_selected(
        &mut self,
        action: FileAction,
        destination: Option<PathBuf>,
        conflict: ConflictPolicy,
        conflict_overrides: HashMap<PathBuf, ConflictPolicy>,
    ) {
        let sources = self
            .selected_records()
            .into_iter()
            .map(|record| record.path)
            .collect();
        let request = FileOperationRequest {
            action,
            sources,
            destination,
            conflict,
            conflict_overrides,
        };
        match self.file_ops.submit(request) {
            Ok(()) => {
                self.file_operation_running = true;
                self.status = "正在执行文件操作…".into();
            }
            Err(error) => self.status = error.to_string(),
        }
    }

    fn grid(&mut self, ui: &mut egui::Ui) {
        let cell_width = self.thumb_size as f32 + 28.0;
        let columns = (ui.available_width().max(1.0) / cell_width)
            .floor()
            .max(1.0) as usize;
        let rows = self.records.len().div_ceil(columns);
        let row_height = self.thumb_size as f32 + 58.0;
        let viewport_height = ui.available_height();

        if let Some(index) = self.pending_grid_focus.take() {
            let row = index.min(self.records.len().saturating_sub(1)) / columns;
            let centered = row as f32 * row_height - (viewport_height - row_height) * 0.5;
            self.pending_grid_scroll_offset = Some(centered.max(0.0));
        }

        let mut scroll_area = egui::ScrollArea::vertical()
            .id_salt("image-grid-scroll")
            .auto_shrink([false; 2]);
        if let Some(offset) = self.pending_grid_scroll_offset.take() {
            scroll_area = scroll_area.vertical_scroll_offset(offset);
        }

        let output = scroll_area.show_rows(ui, row_height, rows, |ui, visible| {
            let visible_count = visible.len().max(1);
            for row in visible.clone() {
                for column in 0..columns {
                    let index = row * columns + column;
                    let Some(record) = self.records.get(index) else {
                        break;
                    };
                    let key = texture_key(record, ImageKind::Thumbnail);
                    if !self.textures.contains_key(&key) && !self.failed_images.contains(&key) {
                        self.thumbnails
                            .request_thumbnail(record.clone(), ThumbnailPriority::Visible);
                    }
                }
            }
            let prefetch_start = visible.start.saturating_sub(visible_count);
            let prefetch_end = (visible.end + visible_count).min(rows);
            let prefetch_rows = (prefetch_start, prefetch_end);
            if self.prefetch_rows != Some(prefetch_rows) {
                self.prefetch_rows = Some(prefetch_rows);
                self.thumbnails.advance_prefetch_epoch();
            }
            for row in prefetch_start..prefetch_end {
                if visible.contains(&row) {
                    continue;
                }
                for column in 0..columns {
                    let index = row * columns + column;
                    let Some(record) = self.records.get(index) else {
                        break;
                    };
                    let key = texture_key(record, ImageKind::Thumbnail);
                    if !self.textures.contains_key(&key) && !self.failed_images.contains(&key) {
                        self.thumbnails
                            .request_thumbnail(record.clone(), ThumbnailPriority::Prefetch);
                    }
                }
            }
            for row in visible {
                ui.horizontal(|ui| {
                    for column in 0..columns {
                        let index = row * columns + column;
                        if index >= self.records.len() {
                            break;
                        }
                        let record = self.records[index].clone();
                        let key = texture_key(&record, ImageKind::Thumbnail);
                        self.touch_texture(&key);
                        ui.allocate_ui_with_layout(
                            egui::vec2(cell_width - 8.0, row_height),
                            egui::Layout::top_down(egui::Align::Center),
                            |ui| {
                                let selected = self.selected.contains(&record.id);
                                let border_color = if selected {
                                    ui.visuals().selection.stroke.color
                                } else {
                                    ui.visuals().weak_text_color()
                                };
                                egui::Frame::new()
                                    .stroke(egui::Stroke::new(
                                        if selected { 2.0 } else { 1.0 },
                                        border_color,
                                    ))
                                    .corner_radius(4.0)
                                    .inner_margin(6.0)
                                    .show(ui, |ui| {
                                        ui.set_min_size(egui::vec2(
                                            cell_width - 22.0,
                                            row_height - 14.0,
                                        ));
                                        ui.with_layout(
                                            egui::Layout::top_down(egui::Align::Center),
                                            |ui| {
                                                let thumbnail_size = egui::vec2(
                                                    self.thumb_size as f32,
                                                    self.thumb_size as f32,
                                                );
                                                let (thumbnail_rect, response) = ui
                                                    .allocate_exact_size(
                                                        thumbnail_size,
                                                        egui::Sense::click(),
                                                    );
                                                if let Some(texture) = self.textures.get(&key) {
                                                    let natural = texture.size_vec2();
                                                    let scale = (self.thumb_size as f32
                                                        / natural.x)
                                                        .min(self.thumb_size as f32 / natural.y);
                                                    let image_rect = egui::Rect::from_center_size(
                                                        thumbnail_rect.center(),
                                                        natural * scale,
                                                    );
                                                    ui.painter().image(
                                                        texture.id(),
                                                        image_rect,
                                                        egui::Rect::from_min_max(
                                                            egui::Pos2::ZERO,
                                                            egui::pos2(1.0, 1.0),
                                                        ),
                                                        egui::Color32::WHITE,
                                                    );
                                                } else if self.failed_images.contains(&key) {
                                                    ui.painter().rect_filled(
                                                        thumbnail_rect,
                                                        4.0,
                                                        ui.visuals().extreme_bg_color,
                                                    );
                                                    ui.painter().text(
                                                        thumbnail_rect.center(),
                                                        egui::Align2::CENTER_CENTER,
                                                        "无法预览",
                                                        egui::FontId::proportional(14.0),
                                                        ui.visuals().weak_text_color(),
                                                    );
                                                } else {
                                                    ui.painter().rect_filled(
                                                        thumbnail_rect,
                                                        4.0,
                                                        ui.visuals().extreme_bg_color,
                                                    );
                                                    ui.painter().text(
                                                        thumbnail_rect.center(),
                                                        egui::Align2::CENTER_CENTER,
                                                        "加载中…",
                                                        egui::FontId::proportional(14.0),
                                                        ui.visuals().weak_text_color(),
                                                    );
                                                }
                                                let response = response.on_hover_text(format!(
                                                    "{}\n{} × {}\n{}\n{}",
                                                    record.path.display(),
                                                    record
                                                        .width
                                                        .map(|value| value.to_string())
                                                        .unwrap_or_else(|| "?".into()),
                                                    record
                                                        .height
                                                        .map(|value| value.to_string())
                                                        .unwrap_or_else(|| "?".into()),
                                                    record.format.to_uppercase(),
                                                    format_bytes(record.size),
                                                ));
                                                response.context_menu(|ui| {
                                                    if ui.button("在资源管理器中显示").clicked()
                                                    {
                                                        let _ = file_ops::reveal_in_explorer(
                                                            &record.path,
                                                        );
                                                        ui.close_menu();
                                                    }
                                                    if ui.button("使用系统默认程序打开").clicked()
                                                    {
                                                        let _ = file_ops::open_with_default(
                                                            &record.path,
                                                        );
                                                        ui.close_menu();
                                                    }
                                                });
                                                if response.double_clicked() {
                                                    self.open_preview(index);
                                                }
                                                let mut checked = selected;
                                                if ui.checkbox(&mut checked, "").changed() {
                                                    if checked {
                                                        self.selected.insert(record.id);
                                                    } else {
                                                        self.selected.remove(&record.id);
                                                    }
                                                    self.selection_anchor = Some(index);
                                                }
                                                ui.add(
                                                    egui::Label::new(
                                                        egui::RichText::new(&record.file_name)
                                                            .small(),
                                                    )
                                                    .truncate(),
                                                );
                                            },
                                        );
                                    });
                            },
                        );
                    }
                });
            }
        });
        self.grid_scroll_offset = output.state.offset.y;
    }

    fn open_preview(&mut self, index: usize) {
        if self.preview.is_none() {
            self.preview_origin = Some(index);
            self.preview_return_offset = self.grid_scroll_offset;
        }
        self.preview = Some(index);
        self.zoom = 1.0;
        self.fit_preview = true;
        self.rotation_quarters = 0;
        let keep: HashSet<String> = (index.saturating_sub(1)
            ..=(index + 1).min(self.records.len().saturating_sub(1)))
            .map(|position| texture_key(&self.records[position], ImageKind::Preview))
            .collect();
        let remove: Vec<String> = self
            .textures
            .keys()
            .filter(|key| key.ends_with(":preview") && !keep.contains(*key))
            .cloned()
            .collect();
        for key in remove {
            if let Some(texture) = self.textures.remove(&key) {
                self.texture_bytes = self
                    .texture_bytes
                    .saturating_sub(texture.size()[0] * texture.size()[1] * 4);
            }
            self.texture_last_used.remove(&key);
        }
    }

    fn close_preview(&mut self) {
        if let Some(current) = self.preview {
            if self.preview_origin == Some(current) {
                self.pending_grid_scroll_offset = Some(self.preview_return_offset);
            } else {
                self.pending_grid_focus = Some(current);
            }
        }
        self.preview = None;
        self.preview_origin = None;
    }

    fn preview_ui(&mut self, ctx: &egui::Context) {
        let Some(index) = self.preview else {
            return;
        };
        if index >= self.records.len() {
            self.preview = None;
            return;
        }
        let record = self.records[index].clone();
        let key = texture_key(&record, ImageKind::Preview);
        if !self.textures.contains_key(&key) && !self.failed_images.contains(&key) {
            self.thumbnails.request_preview(record.clone(), 4096);
        }
        self.touch_texture(&key);

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::from_rgb(18, 18, 20)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("← 上一张").clicked() && index > 0 {
                        self.open_preview(index - 1);
                    }
                    if ui.button("下一张 →").clicked() && index + 1 < self.records.len() {
                        self.open_preview(index + 1);
                    }
                    ui.separator();
                    ui.label(format!(
                        "{} / {}  {}",
                        index + 1,
                        self.records.len(),
                        record.file_name
                    ));
                    if ui.button("适应窗口").clicked() {
                        self.fit_preview = true;
                        self.zoom = 1.0;
                    }
                    if ui.button("100%").clicked() {
                        self.fit_preview = false;
                        self.zoom = 1.0;
                    }
                    if ui.button("旋转 90°").clicked() {
                        self.rotation_quarters = (self.rotation_quarters + 1) % 4;
                    }
                    if ui.button("在文件夹中显示").clicked() {
                        let _ = file_ops::reveal_in_explorer(&record.path);
                    }
                    if ui.button("关闭 (Esc)").clicked() {
                        self.close_preview();
                    }
                });
                ui.separator();
                egui::ScrollArea::both()
                    .drag_to_scroll(true)
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        ui.centered_and_justified(|ui| {
                            if let Some(texture) = self.textures.get(&key) {
                                let available = ui.available_size();
                                let natural = texture.size_vec2();
                                let base_scale = if self.fit_preview {
                                    (available.x / natural.x)
                                        .min(available.y / natural.y)
                                        .min(1.0)
                                } else {
                                    1.0
                                };
                                let size = natural * base_scale * self.zoom;
                                let angle =
                                    self.rotation_quarters as f32 * std::f32::consts::FRAC_PI_2;
                                ui.add(
                                    egui::Image::new((texture.id(), size))
                                        .rotate(angle, egui::vec2(0.5, 0.5)),
                                );
                            } else if self.failed_images.contains(&key) {
                                ui.colored_label(
                                    egui::Color32::LIGHT_RED,
                                    "图片损坏、格式不受支持或没有读取权限。",
                                );
                            } else {
                                ui.vertical_centered(|ui| {
                                    ui.spinner();
                                    ui.label("正在加载大图预览…");
                                });
                            }
                        });
                    });
            });

        let scroll = ctx.input(|input| input.smooth_scroll_delta.y);
        if scroll != 0.0 {
            self.fit_preview = false;
            self.zoom = (self.zoom * (1.0 + scroll * 0.001)).clamp(0.05, 12.0);
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.input(|input| input.key_pressed(egui::Key::F5)) {
            self.refresh();
        }
        if ctx.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::A))
            && self.preview.is_none()
        {
            self.selected = self.records.iter().map(|record| record.id).collect();
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            if self.preview.is_some() {
                self.close_preview();
            } else {
                self.selected.clear();
            }
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Delete))
            && !self.selected.is_empty()
            && self.pending.is_none()
        {
            let permanent = ctx.input(|input| input.modifiers.shift);
            self.pending = Some(PendingDialog::Delete { permanent });
        }
        if self.preview.is_some() {
            if ctx.input(|input| input.key_pressed(egui::Key::ArrowLeft)) {
                if let Some(index) = self.preview.filter(|index| *index > 0) {
                    self.open_preview(index - 1);
                }
            }
            if ctx.input(|input| input.key_pressed(egui::Key::ArrowRight)) {
                if let Some(index) = self.preview.filter(|index| *index + 1 < self.records.len()) {
                    self.open_preview(index + 1);
                }
            }
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.show_about {
            let mut open = true;
            let mut close_clicked = false;
            egui::Window::new("关于图海速览")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.heading(crate::APP_NAME);
                    ui.add_space(8.0);
                    egui::Grid::new("about-information")
                        .num_columns(2)
                        .spacing([18.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("作者：");
                            ui.label("大都督");
                            ui.end_row();
                            ui.label("微信：");
                            ui.label("xmqsvip");
                            ui.end_row();
                            ui.label("版本号：");
                            ui.label(crate::APP_VERSION);
                            ui.end_row();
                        });
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("关闭").clicked() {
                            close_clicked = true;
                        }
                    });
                });
            self.show_about = open && !close_clicked;
        }

        if self.show_empty {
            let mut open = true;
            egui::Window::new("空文件夹预览")
                .open(&mut open)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "找到 {} 个真正为空的子文件夹。根目录不会被删除。",
                        self.empty_folders.len()
                    ));
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            for folder in &self.empty_folders {
                                ui.label(&folder.relative_path);
                            }
                        });
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !self.empty_folders.is_empty(),
                                egui::Button::new("移入回收站"),
                            )
                            .clicked()
                        {
                            self.pending = Some(PendingDialog::DeleteEmpty { permanent: false });
                        }
                        if ui
                            .add_enabled(
                                !self.empty_folders.is_empty(),
                                egui::Button::new("永久删除"),
                            )
                            .clicked()
                        {
                            self.pending = Some(PendingDialog::DeleteEmpty { permanent: true });
                        }
                    });
                });
            self.show_empty = open;
        }

        let Some(dialog) = self.pending.take() else {
            return;
        };
        let mut keep = true;
        match dialog {
            PendingDialog::Delete { permanent } => {
                let count = self.selected.len();
                let total: u64 = self
                    .selected_records()
                    .iter()
                    .map(|record| record.size)
                    .sum();
                let mut confirmed = false;
                egui::Window::new(if permanent {
                    "确认永久删除"
                } else {
                    "确认移入回收站"
                })
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    if permanent {
                        ui.colored_label(egui::Color32::RED, "此操作不可恢复。请确认目标无误。");
                    }
                    ui.label(format!(
                        "将处理 {count} 张图片，共 {}。",
                        format_bytes(total)
                    ));
                    for record in self.selected_records().iter().take(8) {
                        ui.label(record.path.display().to_string());
                    }
                    if count > 8 {
                        ui.label(format!("…以及另外 {} 张", count - 8));
                    }
                    ui.horizontal(|ui| {
                        if ui.button("确认").clicked() {
                            confirmed = true;
                            keep = false;
                        }
                        if ui.button("取消").clicked() {
                            keep = false;
                        }
                    });
                });
                if confirmed {
                    self.submit_selected(
                        if permanent {
                            FileAction::PermanentDelete
                        } else {
                            FileAction::RecycleDelete
                        },
                        None,
                        ConflictPolicy::Skip,
                        HashMap::new(),
                    );
                }
                if keep {
                    self.pending = Some(PendingDialog::Delete { permanent });
                }
            }
            PendingDialog::Transfer(mut transfer) => {
                let mut confirmed = false;
                let mut advance = false;
                let mut cancel = false;
                egui::Window::new(if transfer.action == FileAction::Copy {
                    "确认复制"
                } else {
                    "确认剪切"
                })
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "将 {} {} 张图片到：",
                        if transfer.action == FileAction::Copy {
                            "复制"
                        } else {
                            "移动"
                        },
                        self.selected.len()
                    ));
                    ui.label(transfer.destination.display().to_string());
                    if !transfer.conflicts.is_empty()
                        && transfer.conflict_index < transfer.conflicts.len()
                    {
                        let current = &transfer.conflicts[transfer.conflict_index];
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            format!(
                                "同名冲突 {}/{}",
                                transfer.conflict_index + 1,
                                transfer.conflicts.len()
                            ),
                        );
                        ui.label(current.display().to_string());
                        ui.radio_value(
                            &mut self.conflict_policy,
                            ConflictPolicy::AutoRename,
                            "自动重命名并保留两份",
                        );
                        ui.radio_value(
                            &mut self.conflict_policy,
                            ConflictPolicy::Skip,
                            "跳过同名文件",
                        );
                        ui.radio_value(
                            &mut self.conflict_policy,
                            ConflictPolicy::Overwrite,
                            "覆盖目标文件",
                        );
                        ui.checkbox(&mut transfer.apply_to_remaining, "应用到全部剩余冲突");
                    }
                    ui.horizontal(|ui| {
                        let has_more = transfer.conflict_index + 1 < transfer.conflicts.len()
                            && !transfer.apply_to_remaining;
                        if ui
                            .button(if has_more { "下一个" } else { "开始" })
                            .clicked()
                        {
                            if transfer.conflicts.is_empty() {
                                confirmed = true;
                            } else {
                                let current = transfer.conflicts[transfer.conflict_index].clone();
                                transfer.decisions.insert(current, self.conflict_policy);
                                if has_more {
                                    advance = true;
                                } else {
                                    confirmed = true;
                                }
                            }
                        }
                        if ui.button("取消").clicked() {
                            cancel = true;
                        }
                    });
                });
                if confirmed {
                    let default_policy = if transfer.apply_to_remaining {
                        self.conflict_policy
                    } else {
                        ConflictPolicy::AutoRename
                    };
                    let decisions = std::mem::take(&mut transfer.decisions);
                    self.submit_selected(
                        transfer.action,
                        Some(transfer.destination.clone()),
                        default_policy,
                        decisions,
                    );
                    keep = false;
                } else if advance {
                    transfer.conflict_index += 1;
                } else if cancel {
                    keep = false;
                }
                if keep {
                    self.pending = Some(PendingDialog::Transfer(transfer));
                }
            }
            PendingDialog::DeleteEmpty { permanent } => {
                let valid: Vec<_> = self
                    .empty_folders
                    .iter()
                    .filter(|folder| {
                        fs::read_dir(&folder.path)
                            .map(|mut entries| entries.next().is_none())
                            .unwrap_or(false)
                            && self.root.as_ref().is_some_and(|root| folder.path != *root)
                    })
                    .cloned()
                    .collect();
                let mut confirmed = false;
                egui::Window::new("确认删除空文件夹")
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.label(format!(
                            "复核后仍有 {} 个空文件夹，将从最深层开始删除。",
                            valid.len()
                        ));
                        if permanent {
                            ui.colored_label(egui::Color32::RED, "永久删除不可恢复。");
                        }
                        ui.horizontal(|ui| {
                            if ui.button("确认").clicked() {
                                confirmed = true;
                                keep = false;
                            }
                            if ui.button("取消").clicked() {
                                keep = false;
                            }
                        });
                    });
                if confirmed {
                    let mut sources: Vec<_> = valid.into_iter().map(|folder| folder.path).collect();
                    sources.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
                    let request = FileOperationRequest {
                        action: if permanent {
                            FileAction::PermanentDelete
                        } else {
                            FileAction::RecycleDelete
                        },
                        sources,
                        destination: None,
                        conflict: ConflictPolicy::Skip,
                        conflict_overrides: HashMap::new(),
                    };
                    if self.file_ops.submit(request).is_ok() {
                        self.file_operation_running = true;
                        self.show_empty = false;
                        self.status = "正在删除空文件夹…".into();
                    }
                }
                if keep {
                    self.pending = Some(PendingDialog::DeleteEmpty { permanent });
                }
            }
            PendingDialog::ClearStorage => {
                let data_dir = self
                    .catalog
                    .data_dir()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|_| "程序目录\\data".into());
                let mut confirmed = false;
                egui::Window::new("确认清理本地数据")
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "将删除缩略图缓存和图片索引数据库。",
                        );
                        ui.label("不会删除任何原始图片或文件夹。下次扫描会重新建立索引和缩略图。");
                        ui.label(format!("数据目录：{data_dir}"));
                        ui.horizontal(|ui| {
                            if ui.button("确认清理").clicked() {
                                confirmed = true;
                                keep = false;
                            }
                            if ui.button("取消").clicked() {
                                keep = false;
                            }
                        });
                    });
                if confirmed {
                    let database = self.catalog.clear_database();
                    let thumbnails = self.thumbnails.clear_disk_cache();
                    let legacy = crate::storage::clear_legacy_storage();
                    match (database, thumbnails, legacy) {
                        (Ok(()), Ok(()), Ok(())) => {
                            self.records.clear();
                            self.record_positions.clear();
                            self.data_revision = self.data_revision.wrapping_add(1);
                            self.selected.clear();
                            self.selection_anchor = None;
                            self.textures.clear();
                            self.texture_last_used.clear();
                            self.texture_clock = 0;
                            self.texture_bytes = 0;
                            self.failed_images.clear();
                            self.status = "缓存和数据库已清理，正在重新扫描…".into();
                            self.refresh();
                        }
                        (database, thumbnails, legacy) => {
                            let mut errors = Vec::new();
                            if let Err(error) = database {
                                errors.push(format!("数据库：{error}"));
                            }
                            if let Err(error) = thumbnails {
                                errors.push(format!("缓存：{error}"));
                            }
                            if let Err(error) = legacy {
                                errors.push(format!("旧版 C 盘数据：{error}"));
                            }
                            self.status = format!("清理失败：{}", errors.join("；"));
                        }
                    }
                }
                if keep {
                    self.pending = Some(PendingDialog::ClearStorage);
                }
            }
        }
    }
}

impl eframe::App for PreviewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_events(ctx);
        self.handle_shortcuts(ctx);

        if self.preview.is_some() {
            self.preview_ui(ctx);
        } else {
            egui::TopBottomPanel::top("toolbar").show(ctx, |ui| self.toolbar(ui));
            egui::TopBottomPanel::bottom("batch").show(ctx, |ui| self.batch_bar(ui));
            egui::CentralPanel::default().show(ctx, |ui| {
                if self.root.is_none() {
                    ui.centered_and_justified(|ui| {
                        ui.vertical_centered(|ui| {
                            ui.heading("图海速览");
                            ui.label("递归浏览上万张图片，并安全执行批量文件操作");
                            if ui.button("选择图片文件夹").clicked() {
                                self.choose_root();
                            }
                        });
                    });
                } else if self.records.is_empty() && !self.scanning {
                    ui.centered_and_justified(|ui| {
                        ui.label("此文件夹及子文件夹中没有支持的图片。")
                    });
                } else {
                    self.grid(ui);
                }
            });
        }
        self.dialogs(ctx);
        if self.scanning
            || self.empty_folder_scanning
            || self.file_operation_running
            || self.sorting
        {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if let Some(due) = self.rescan_due {
            ctx.request_repaint_after(due.saturating_duration_since(Instant::now()));
        }
    }

    fn on_exit(&mut self) {
        self.catalog.cancel_scan();
        self.empty_folder_service.cancel();
        self.textures.clear();
        self.texture_last_used.clear();
        self.texture_clock = 0;
        self.texture_bytes = 0;
    }
}

fn configure_chinese_font(ctx: &egui::Context) {
    let candidates = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\simhei.ttf",
    ];
    let Some(bytes) = candidates.iter().find_map(|path| fs::read(path).ok()) else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "windows-cjk".into(),
        egui::FontData::from_owned(bytes).into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "windows-cjk".into());
    }
    ctx.set_fonts(fonts);
}

fn sort_label(sort: SortMode) -> &'static str {
    match sort {
        SortMode::ModifiedDesc => "修改时间↓",
        SortMode::NameNatural => "文件名自然排序",
        SortMode::SizeDesc => "文件大小↓",
        SortMode::Path => "文件夹路径",
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.2} GiB", value / GB)
    } else if value >= MB {
        format!("{:.1} MiB", value / MB)
    } else if value >= KB {
        format!("{:.1} KiB", value / KB)
    } else {
        format!("{bytes} B")
    }
}
