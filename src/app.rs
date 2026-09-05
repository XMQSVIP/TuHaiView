use crate::{
    catalog::{CatalogEvent, CatalogService, MetadataUpdate},
    duplicates::{
        DuplicateEvent, DuplicateGroup, DuplicateService, DuplicateStats, validate_delete_candidate,
    },
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

const WECHAT_DONATION_CODE: &[u8] = include_bytes!("../assets/wechat-donation-code.jpg");

enum PendingDialog {
    Delete { permanent: bool },
    Transfer(TransferDialog),
    DeleteEmpty { permanent: bool },
    ClearStorage,
    DuplicateDelete(DuplicateDeleteDialog),
}

struct TransferDialog {
    action: FileAction,
    destination: PathBuf,
    conflicts: Vec<PathBuf>,
    conflict_index: usize,
    decisions: HashMap<PathBuf, ConflictPolicy>,
    apply_to_remaining: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DuplicateDeleteMode {
    KeepOne,
    DeleteAll,
}

struct DuplicateDeleteDialog {
    mode: DuplicateDeleteMode,
    permanent_stage: bool,
    confirmation_text: String,
}

struct DuplicateViewState {
    groups: Vec<DuplicateGroup>,
    stats: DuplicateStats,
    errors: Vec<(PathBuf, String)>,
    page: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuxiliaryWindow {
    About,
    EmptyFolders,
    Duplicates,
}

pub struct PreviewerApp {
    catalog: CatalogService,
    duplicate_service: DuplicateService,
    empty_folder_service: EmptyFolderService,
    thumbnails: ThumbnailService,
    file_ops: FileOperationService,
    sort_service: SortService,
    root: Option<PathBuf>,
    generation: u64,
    records: Vec<ImageRecord>,
    record_positions: HashMap<PathBuf, usize>,
    display_indices: Vec<usize>,
    display_positions: HashMap<i64, usize>,
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
    duplicate_task_id: u64,
    duplicate_scanning: bool,
    duplicate_stats: DuplicateStats,
    duplicate_view: Option<DuplicateViewState>,
    deduplicated_view: bool,
    show_duplicates: bool,
    duplicate_delete_running: bool,
    duplicate_validation_skipped: usize,
    duplicate_operation_errors: Vec<(PathBuf, String)>,
    duplicate_rescan_after_catalog: bool,
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
    donation_texture: Option<TextureHandle>,
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
            duplicate_service: DuplicateService::new(wakeup.clone()),
            empty_folder_service: EmptyFolderService::new(wakeup.clone()),
            thumbnails: ThumbnailService::new(wakeup.clone()),
            file_ops: FileOperationService::new(wakeup.clone()),
            sort_service: SortService::new(wakeup),
            root: None,
            generation: 0,
            records: Vec::new(),
            record_positions: HashMap::new(),
            display_indices: Vec::new(),
            display_positions: HashMap::new(),
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
            duplicate_task_id: 0,
            duplicate_scanning: false,
            duplicate_stats: DuplicateStats::default(),
            duplicate_view: None,
            deduplicated_view: false,
            show_duplicates: false,
            duplicate_delete_running: false,
            duplicate_validation_skipped: 0,
            duplicate_operation_errors: Vec::new(),
            duplicate_rescan_after_catalog: false,
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
            donation_texture: None,
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

    fn donation_texture(&mut self, ctx: &egui::Context) -> Option<TextureHandle> {
        if self.donation_texture.is_none() {
            let decoded = image::load_from_memory(WECHAT_DONATION_CODE).ok()?;
            let rgba = decoded.to_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            let color_image = ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
            self.donation_texture = Some(ctx.load_texture(
                "wechat-donation-code",
                color_image,
                egui::TextureOptions::LINEAR,
            ));
        }
        self.donation_texture.clone()
    }

    /// 顶部菜单对应的辅助窗口互斥显示，避免多个结果窗口叠在一起。
    fn open_auxiliary_window(&mut self, window: AuxiliaryWindow) {
        self.show_about = window == AuxiliaryWindow::About;
        self.show_empty = window == AuxiliaryWindow::EmptyFolders;
        self.show_duplicates = window == AuxiliaryWindow::Duplicates;
    }

    fn close_auxiliary_windows(&mut self) {
        self.show_about = false;
        self.show_empty = false;
        self.show_duplicates = false;
    }

    fn open_root(&mut self, path: PathBuf) {
        // 切换根目录时重置仅属于旧目录的 UI 状态；后台服务用 generation 过滤迟到事件。
        self.close_auxiliary_windows();
        self.duplicate_rescan_after_catalog = false;
        self.duplicate_operation_errors.clear();
        self.invalidate_duplicates();
        self.empty_folder_generation = self.empty_folder_service.cancel();
        self.empty_folder_scanning = false;
        self.show_empty = false;
        self.empty_folders.clear();
        self.root = Some(path.clone());
        self.records.clear();
        self.record_positions.clear();
        self.display_indices.clear();
        self.display_positions.clear();
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
            let reopen_duplicates = self.duplicate_rescan_after_catalog;
            self.invalidate_duplicates();
            if reopen_duplicates && !self.show_about && !self.show_empty {
                self.open_auxiliary_window(AuxiliaryWindow::Duplicates);
            }
            self.scanning = true;
            self.status = "正在增量校验…".into();
            self.catalog.scan(root, self.sort);
        }
    }

    fn invalidate_duplicates(&mut self) {
        let preview_ids = self.preview_record_ids();
        self.duplicate_task_id = self.duplicate_service.cancel();
        self.duplicate_scanning = false;
        self.duplicate_view = None;
        self.deduplicated_view = false;
        self.show_duplicates = false;
        self.duplicate_stats = DuplicateStats::default();
        self.rebuild_display_indices();
        self.restore_preview_by_ids(preview_ids);
    }

    fn start_duplicate_scan(&mut self) {
        if self.scanning || self.file_operation_running || self.records.len() < 2 {
            return;
        }
        let preview_ids = self.preview_record_ids();
        self.deduplicated_view = false;
        self.rebuild_display_indices();
        self.restore_preview_by_ids(preview_ids);
        self.duplicate_view = None;
        self.open_auxiliary_window(AuxiliaryWindow::Duplicates);
        self.duplicate_stats = DuplicateStats::default();
        self.duplicate_task_id = self
            .duplicate_service
            .scan(self.generation, self.records.clone());
        self.duplicate_scanning = true;
        self.status = "正在查找内容完全相同的图片…".into();
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
                    self.rebuild_display_indices();
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
                        self.rebuild_display_indices();
                    }
                }
                CatalogEvent::RemoveBatch { generation, ids } if generation == self.generation => {
                    let preview_ids = self.preview_record_ids();
                    let removed = ids.into_iter().collect::<HashSet<_>>();
                    self.records.retain(|record| !removed.contains(&record.id));
                    self.selected.retain(|id| !removed.contains(id));
                    self.data_revision = self.data_revision.wrapping_add(1);
                    self.rebuild_record_positions();
                    self.rebuild_display_indices();
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
                    if self.duplicate_rescan_after_catalog {
                        self.duplicate_rescan_after_catalog = false;
                        self.start_duplicate_scan();
                    }
                }
                CatalogEvent::Error {
                    generation,
                    message,
                } if generation == self.generation => {
                    self.scanning = false;
                    self.status = format!("扫描失败：{message}");
                }
                CatalogEvent::Changed { generation } if generation == self.generation => {
                    // 文件监控可能在一次复制/移动中产生很多事件，合并后再做一次增量校验。
                    if !self.duplicate_delete_running && !self.duplicate_rescan_after_catalog {
                        self.duplicate_rescan_after_catalog = false;
                        self.duplicate_operation_errors.clear();
                        self.invalidate_duplicates();
                        self.status = "目录内容发生变化，原查重结果已失效，正在增量校验…".into();
                    }
                    self.rescan_due = Some(Instant::now() + Duration::from_millis(700));
                }
                _ => {}
            }
        }

        while let Ok(event) = self.duplicate_service.rx.try_recv() {
            match event {
                DuplicateEvent::Started {
                    generation,
                    task_id,
                    candidate_files,
                } if generation == self.generation && task_id == self.duplicate_task_id => {
                    self.duplicate_stats.candidate_files = candidate_files;
                }
                DuplicateEvent::Progress {
                    generation,
                    task_id,
                    stats,
                } if generation == self.generation && task_id == self.duplicate_task_id => {
                    self.duplicate_stats = stats;
                }
                DuplicateEvent::HashBatch {
                    generation,
                    task_id,
                    updates,
                } if generation == self.generation && task_id == self.duplicate_task_id => {
                    for update in updates {
                        if let Some(record) = self
                            .record_positions
                            .get(&update.path)
                            .copied()
                            .and_then(|index| self.records.get_mut(index))
                            && record.id == update.id
                            && record.size == update.size
                            && record.modified_ns == update.modified_ns
                        {
                            record.content_hash = Some(update.content_hash);
                        }
                    }
                }
                DuplicateEvent::Finished {
                    generation,
                    task_id,
                    groups,
                    stats,
                    mut errors,
                } if generation == self.generation && task_id == self.duplicate_task_id => {
                    self.duplicate_scanning = false;
                    self.duplicate_stats = stats.clone();
                    self.status = format!(
                        "查重完成：发现 {} 组重复图片，复用哈希 {}，新计算 {}，错误 {}，耗时 {:.2}s",
                        groups.len(),
                        stats.reused_hashes,
                        stats.hashed_files,
                        stats.errors,
                        stats.elapsed_ms as f64 / 1000.0,
                    );
                    errors.append(&mut self.duplicate_operation_errors);
                    self.duplicate_view = Some(DuplicateViewState {
                        groups,
                        stats,
                        errors,
                        page: 0,
                    });
                    if !self.show_about && !self.show_empty {
                        self.open_auxiliary_window(AuxiliaryWindow::Duplicates);
                    }
                }
                DuplicateEvent::Cancelled {
                    generation,
                    task_id,
                    stats,
                } if generation == self.generation && task_id == self.duplicate_task_id => {
                    self.duplicate_scanning = false;
                    self.show_duplicates = false;
                    self.duplicate_stats = stats;
                    self.status = "已取消重复图片扫描".into();
                }
                DuplicateEvent::Error {
                    generation,
                    task_id,
                    message,
                } if generation == self.generation && task_id == self.duplicate_task_id => {
                    self.duplicate_scanning = false;
                    self.show_duplicates = false;
                    self.status = format!("查找重复图片失败：{message}");
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
                    if !self.show_about && !self.show_duplicates {
                        self.open_auxiliary_window(AuxiliaryWindow::EmptyFolders);
                    }
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
            let was_duplicate_delete = self.duplicate_delete_running
                && matches!(
                    action,
                    FileAction::RecycleDelete | FileAction::PermanentDelete
                );
            self.duplicate_delete_running = false;
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
                if !success_ids.is_empty() {
                    self.duplicate_task_id = self.duplicate_service.cancel();
                    self.duplicate_scanning = false;
                    self.duplicate_view = None;
                    self.deduplicated_view = false;
                    self.show_duplicates = false;
                    self.duplicate_stats = DuplicateStats::default();
                }
                self.rebuild_record_positions();
                self.rebuild_display_indices();
                self.restore_preview_by_ids(preview_ids);
            }
            self.status = format!(
                "操作完成：成功 {}，跳过 {}，失败 {}{}{}",
                report.succeeded.len(),
                report.skipped.len() + self.duplicate_validation_skipped,
                report.failed.len(),
                if report.cancelled {
                    "，用户取消了部分操作"
                } else {
                    ""
                },
                if was_duplicate_delete {
                    "，正在重新校验重复项"
                } else {
                    ""
                },
            );
            self.duplicate_validation_skipped = 0;
            if was_duplicate_delete {
                self.duplicate_operation_errors
                    .extend(report.skipped.iter().cloned());
                self.duplicate_operation_errors
                    .extend(report.failed.iter().cloned());
                self.duplicate_task_id = self.duplicate_service.cancel();
                self.duplicate_scanning = false;
                self.duplicate_view = None;
                if !self.show_about && !self.show_empty {
                    self.open_auxiliary_window(AuxiliaryWindow::Duplicates);
                }
                self.duplicate_rescan_after_catalog = true;
            }
            if let Some(root) = self.root.clone() {
                self.rescan_due = Some(Instant::now() + Duration::from_millis(800));
                if report.failed.is_empty() && action == FileAction::Copy && root.exists() {
                    ctx.request_repaint();
                }
            }
        }

        while let Ok(mut result) = self.sort_service.rx.try_recv() {
            if result.generation == self.generation
                && result.revision == self.data_revision
                && result.mode == self.sort
            {
                let preview_ids = self.preview_record_ids();
                // 排序快照生成后，缩略图尺寸或内容哈希可能刚好完成；应用排序前合并这些渐进字段。
                for record in &mut result.records {
                    if let Some(current) = self
                        .record_positions
                        .get(&record.path)
                        .copied()
                        .and_then(|index| self.records.get(index))
                        && current.size == record.size
                        && current.modified_ns == record.modified_ns
                    {
                        record.width = current.width;
                        record.height = current.height;
                        record.content_hash.clone_from(&current.content_hash);
                    }
                }
                self.records = result.records;
                self.rebuild_record_positions();
                self.rebuild_display_indices();
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

    fn rebuild_display_indices(&mut self) {
        self.display_indices = build_display_indices(
            &self.records,
            self.duplicate_view
                .as_ref()
                .map(|view| view.groups.as_slice()),
            self.deduplicated_view,
        );
        self.display_positions.clear();
        self.display_positions.reserve(self.display_indices.len());
        for (display_index, record_index) in self.display_indices.iter().copied().enumerate() {
            if let Some(record) = self.records.get(record_index) {
                self.display_positions.insert(record.id, display_index);
            }
        }
        if self.deduplicated_view {
            retain_visible_selection(&mut self.selected, &self.records, &self.display_indices);
        }
        self.selection_anchor = None;
        self.prefetch_rows = None;
        self.thumbnails.advance_prefetch_epoch();
    }

    fn set_deduplicated_view(&mut self, enabled: bool) {
        let can_enable = self
            .duplicate_view
            .as_ref()
            .is_some_and(|view| !view.groups.is_empty());
        let enabled = enabled && can_enable && !self.duplicate_scanning;
        if self.deduplicated_view == enabled {
            return;
        }
        let mut preview_ids = self.preview_record_ids();
        self.deduplicated_view = enabled;
        if enabled {
            preview_ids.0 = preview_ids.0.map(|id| self.duplicate_representative_id(id));
            preview_ids.1 = preview_ids.1.map(|id| self.duplicate_representative_id(id));
        }
        self.rebuild_display_indices();
        self.restore_preview_by_ids(preview_ids);
        self.status = if enabled {
            format!(
                "已开启去重显示：显示 {} / 共 {} 张，隐藏 {} 张重复副本",
                self.display_indices.len(),
                self.records.len(),
                self.records
                    .len()
                    .saturating_sub(self.display_indices.len()),
            )
        } else {
            format!("已关闭去重显示：显示全部 {} 张图片", self.records.len())
        };
    }

    fn duplicate_representative_id(&self, id: i64) -> i64 {
        self.duplicate_view
            .as_ref()
            .and_then(|view| {
                view.groups
                    .iter()
                    .find(|group| group.members.iter().any(|record| record.id == id))
                    .map(|group| group.keeper_id)
            })
            .unwrap_or(id)
    }

    fn refresh_deduplicated_view(&mut self) {
        let mut preview_ids = self.preview_record_ids();
        if self.deduplicated_view {
            preview_ids.0 = preview_ids.0.map(|id| self.duplicate_representative_id(id));
            preview_ids.1 = preview_ids.1.map(|id| self.duplicate_representative_id(id));
        }
        self.rebuild_display_indices();
        self.restore_preview_by_ids(preview_ids);
    }

    fn display_record(&self, display_index: usize) -> Option<&ImageRecord> {
        self.display_indices
            .get(display_index)
            .and_then(|record_index| self.records.get(*record_index))
    }

    fn preview_record_ids(&self) -> (Option<i64>, Option<i64>) {
        let current = self
            .preview
            .and_then(|index| self.display_record(index))
            .map(|record| record.id);
        let origin = self
            .preview_origin
            .and_then(|index| self.display_record(index))
            .map(|record| record.id);
        (current, origin)
    }

    fn restore_preview_by_ids(&mut self, (current, origin): (Option<i64>, Option<i64>)) {
        self.preview = current.and_then(|id| self.display_positions.get(&id).copied());
        self.preview_origin = origin.and_then(|id| self.display_positions.get(&id).copied());
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
        let duplicate_groups = self
            .duplicate_view
            .as_ref()
            .map_or(0, |view| view.groups.len());
        let has_duplicate_result = self.duplicate_view.is_some();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !self.file_operation_running,
                    egui::Button::new("选择文件夹"),
                )
                .clicked()
            {
                self.close_auxiliary_windows();
                self.choose_root();
            }
            if ui
                .add_enabled(
                    self.root.is_some()
                        && !self.scanning
                        && !self.file_operation_running
                        && !self.duplicate_scanning,
                    egui::Button::new("刷新 (F5)"),
                )
                .clicked()
            {
                self.close_auxiliary_windows();
                self.refresh();
            }
            if ui
                .add_enabled(
                    self.root.is_some() && !self.empty_folder_scanning && !self.duplicate_scanning,
                    egui::Button::new("扫描空文件夹"),
                )
                .clicked()
            {
                self.close_auxiliary_windows();
                if let Some(root) = self.root.clone() {
                    self.empty_folder_generation = self.empty_folder_service.scan(root);
                    self.empty_folder_scanning = true;
                    self.empty_folder_visited = 0;
                    self.empty_folder_found = 0;
                    self.empty_folder_errors = 0;
                    self.status = "正在后台扫描空文件夹…".into();
                }
            }
            if ui
                .add_enabled(
                    self.root.is_some()
                        && !self.scanning
                        && !self.file_operation_running
                        && !self.empty_folder_scanning
                        && !self.duplicate_scanning
                        && self.records.len() >= 2,
                    egui::Button::new(if has_duplicate_result {
                        format!("查看重复图片（{duplicate_groups} 组）")
                    } else {
                        "查找重复图片".into()
                    }),
                )
                .clicked()
            {
                if has_duplicate_result {
                    self.open_auxiliary_window(AuxiliaryWindow::Duplicates);
                } else {
                    self.duplicate_operation_errors.clear();
                    self.start_duplicate_scan();
                }
            }
            if self.duplicate_scanning && ui.button("取消查重").clicked() {
                self.duplicate_task_id = self.duplicate_service.cancel();
                self.duplicate_scanning = false;
                self.show_duplicates = false;
                self.status = format!(
                    "已取消查重：检查 {} / {}，读取 {}",
                    self.duplicate_stats.checked_files,
                    self.duplicate_stats.candidate_files,
                    format_bytes(self.duplicate_stats.bytes_read),
                );
            }
            let filter_response = ui
                .add_enabled(
                    duplicate_groups > 0 && !self.duplicate_scanning,
                    egui::Button::new(if self.deduplicated_view {
                        "✓ 重复副本只显示一张"
                    } else {
                        "重复副本只显示一张"
                    })
                    .selected(self.deduplicated_view),
                )
                .on_hover_text("仅影响主界面显示，不会删除文件。")
                .on_disabled_hover_text("请先完成查重，并确认存在重复图片。");
            if filter_response.clicked() {
                self.set_deduplicated_view(!self.deduplicated_view);
            }
            if self.deduplicated_view {
                ui.label(format!(
                    "显示 {} / 共 {} 张，隐藏 {} 张重复副本",
                    self.display_indices.len(),
                    self.records.len(),
                    self.records
                        .len()
                        .saturating_sub(self.display_indices.len()),
                ));
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
                    !self.scanning && !self.file_operation_running && !self.duplicate_scanning,
                    egui::Button::new("清理缓存和数据库"),
                )
                .clicked()
            {
                self.close_auxiliary_windows();
                self.pending = Some(PendingDialog::ClearStorage);
            }
            if ui.button("关于").clicked() {
                self.open_auxiliary_window(AuxiliaryWindow::About);
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
            if self.duplicate_scanning {
                ui.spinner();
                ui.label(format!(
                    "查重：检查 {}/{}，读取 {}，重复组 {}，错误 {}，耗时 {:.1}s",
                    self.duplicate_stats.checked_files,
                    self.duplicate_stats.candidate_files,
                    format_bytes(self.duplicate_stats.bytes_read),
                    self.duplicate_stats.duplicate_groups,
                    self.duplicate_stats.errors,
                    self.duplicate_stats.elapsed_ms as f64 / 1000.0,
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
                .add_enabled(
                    !self.file_operation_running && !self.duplicate_scanning,
                    egui::Button::new("复制到…"),
                )
                .clicked()
            {
                self.prepare_transfer(FileAction::Copy);
            }
            if ui
                .add_enabled(
                    !self.file_operation_running && !self.duplicate_scanning,
                    egui::Button::new("剪切到…"),
                )
                .clicked()
            {
                self.prepare_transfer(FileAction::Move);
            }
            if ui
                .add_enabled(
                    !self.file_operation_running && !self.duplicate_scanning,
                    egui::Button::new("移入回收站 (Delete)"),
                )
                .clicked()
            {
                self.pending = Some(PendingDialog::Delete { permanent: false });
            }
            if ui
                .add_enabled(
                    !self.file_operation_running && !self.duplicate_scanning,
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
                if matches!(
                    action,
                    FileAction::Move | FileAction::RecycleDelete | FileAction::PermanentDelete
                ) {
                    self.set_deduplicated_view(false);
                }
                self.file_operation_running = true;
                self.status = "正在执行文件操作…".into();
            }
            Err(error) => self.status = error.to_string(),
        }
    }

    fn duplicate_target_summary(&self, mode: DuplicateDeleteMode) -> (usize, usize, u64) {
        let Some(view) = &self.duplicate_view else {
            return (0, 0, 0);
        };
        let mut group_count = 0;
        let mut file_count = 0;
        let mut total_size = 0_u64;
        for group in view.groups.iter().filter(|group| group.included) {
            group_count += 1;
            for record in &group.members {
                if mode == DuplicateDeleteMode::KeepOne && record.id == group.keeper_id {
                    continue;
                }
                file_count += 1;
                total_size = total_size.saturating_add(record.size);
            }
        }
        (group_count, file_count, total_size)
    }

    fn duplicate_targets(&self, mode: DuplicateDeleteMode) -> Vec<(ImageRecord, String)> {
        self.duplicate_view
            .as_ref()
            .into_iter()
            .flat_map(|view| view.groups.iter())
            .filter(|group| group.included)
            .flat_map(|group| {
                group
                    .members
                    .iter()
                    .filter(move |record| {
                        mode == DuplicateDeleteMode::DeleteAll || record.id != group.keeper_id
                    })
                    .cloned()
                    .map(|record| (record, group.hash.clone()))
            })
            .collect()
    }

    fn submit_duplicate_delete(&mut self, mode: DuplicateDeleteMode, action: FileAction) {
        if self.duplicate_scanning || self.file_operation_running {
            return;
        }
        let mut sources = Vec::new();
        let mut skipped = Vec::<(PathBuf, String)>::new();
        for (scanned, expected_hash) in self.duplicate_targets(mode) {
            let current = self
                .record_positions
                .get(&scanned.path)
                .copied()
                .and_then(|index| self.records.get(index));
            let validation = current
                .ok_or_else(|| "图片已不在当前索引中".to_owned())
                .and_then(|record| {
                    if record.id != scanned.id
                        || record.size != scanned.size
                        || record.modified_ns != scanned.modified_ns
                    {
                        Err("图片索引已发生变化".to_owned())
                    } else {
                        validate_delete_candidate(record, &expected_hash)
                            .map_err(|error| error.to_string())
                    }
                });
            match validation {
                Ok(()) => sources.push(scanned.path),
                Err(error) => skipped.push((scanned.path, error)),
            }
        }
        self.duplicate_validation_skipped = skipped.len();
        self.duplicate_operation_errors.extend(skipped);
        if sources.is_empty() {
            self.status = if self.duplicate_validation_skipped == 0 {
                "没有可删除的重复图片".into()
            } else {
                format!(
                    "删除前复核未通过，已跳过 {} 张图片",
                    self.duplicate_validation_skipped
                )
            };
            return;
        }
        let request = FileOperationRequest {
            action,
            sources,
            destination: None,
            conflict: ConflictPolicy::Skip,
            conflict_overrides: HashMap::new(),
        };
        match self.file_ops.submit(request) {
            Ok(()) => {
                self.set_deduplicated_view(false);
                self.file_operation_running = true;
                self.duplicate_delete_running = true;
                self.show_duplicates = false;
                self.status = if action == FileAction::PermanentDelete {
                    "正在永久删除重复图片…".into()
                } else {
                    "正在将重复图片移入回收站…".into()
                };
            }
            Err(error) => self.status = error.to_string(),
        }
    }

    fn grid(&mut self, ui: &mut egui::Ui) {
        let available_width = ui.available_width().max(1.0);
        let (columns, cell_width) = grid_layout(
            available_width,
            self.thumb_size as f32,
            ui.spacing().item_spacing.x,
        );
        let rows = self.display_indices.len().div_ceil(columns);
        let row_height = self.thumb_size as f32 + 58.0;
        let viewport_height = ui.available_height();

        if let Some(index) = self.pending_grid_focus.take() {
            let row = index.min(self.display_indices.len().saturating_sub(1)) / columns;
            let centered = row as f32 * row_height - (viewport_height - row_height) * 0.5;
            self.pending_grid_scroll_offset = Some(centered.max(0.0));
        }

        let mut scroll_area = egui::ScrollArea::vertical()
            .id_salt("image-grid-scroll")
            .auto_shrink([false; 2]);
        if let Some(offset) = self.pending_grid_scroll_offset.take() {
            scroll_area = scroll_area.vertical_scroll_offset(offset);
        }

        // show_rows 只创建可见行的控件，是数万张图片仍能顺畅滚动的关键。
        let output = scroll_area.show_rows(ui, row_height, rows, |ui, visible| {
            let visible_count = visible.len().max(1);
            for row in visible.clone() {
                for column in 0..columns {
                    let index = row * columns + column;
                    let Some(record) = self.display_record(index).cloned() else {
                        break;
                    };
                    let key = texture_key(&record, ImageKind::Thumbnail);
                    if !self.textures.contains_key(&key) && !self.failed_images.contains(&key) {
                        self.thumbnails
                            .request_thumbnail(record, ThumbnailPriority::Visible);
                    }
                }
            }
            if !self.duplicate_scanning {
                // 预取范围保持在视口上下各一屏；范围变化时让旧预取任务失效。
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
                        let Some(record) = self.display_record(index).cloned() else {
                            break;
                        };
                        let key = texture_key(&record, ImageKind::Thumbnail);
                        if !self.textures.contains_key(&key) && !self.failed_images.contains(&key) {
                            self.thumbnails
                                .request_thumbnail(record, ThumbnailPriority::Prefetch);
                        }
                    }
                }
            }
            for row in visible {
                ui.horizontal(|ui| {
                    for column in 0..columns {
                        let index = row * columns + column;
                        if index >= self.display_indices.len() {
                            break;
                        }
                        let Some(record) = self.display_record(index).cloned() else {
                            break;
                        };
                        let key = texture_key(&record, ImageKind::Thumbnail);
                        self.touch_texture(&key);
                        ui.allocate_ui_with_layout(
                            egui::vec2(cell_width, row_height),
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
                                        if selected { 2.0_f32 } else { 1.0_f32 },
                                        border_color,
                                    ))
                                    .corner_radius(4.0)
                                    .inner_margin(6.0)
                                    .show(ui, |ui| {
                                        ui.set_min_size(egui::vec2(
                                            cell_width - 14.0,
                                            row_height - 14.0,
                                        ));
                                        ui.with_layout(
                                            egui::Layout::top_down(egui::Align::Center),
                                            |ui| {
                                                let thumbnail_size = egui::vec2(
                                                    self.thumb_size as f32,
                                                    self.thumb_size as f32,
                                                );
                                                // 为所有图片保留同尺寸槽位：小图只居中绘制，
                                                // 因而同一行的复选框和文件名始终对齐。
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
            // 记住网格实际滚动偏移，关闭大图预览后回到用户打开的那一行。
            self.preview_origin = Some(index);
            self.preview_return_offset = self.grid_scroll_offset;
        }
        self.preview = Some(index);
        self.zoom = 1.0;
        self.fit_preview = true;
        self.rotation_quarters = 0;
        let keep: HashSet<String> = (index.saturating_sub(1)
            ..=(index + 1).min(self.display_indices.len().saturating_sub(1)))
            .filter_map(|position| self.display_record(position))
            .map(|record| texture_key(record, ImageKind::Preview))
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
        if index >= self.display_indices.len() {
            self.preview = None;
            return;
        }
        let Some(record) = self.display_record(index).cloned() else {
            self.preview = None;
            return;
        };
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
                    if ui.button("下一张 →").clicked() && index + 1 < self.display_indices.len()
                    {
                        self.open_preview(index + 1);
                    }
                    ui.separator();
                    ui.label(format!(
                        "{} / {}  {}",
                        index + 1,
                        self.display_indices.len(),
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
        if ctx.input(|input| input.key_pressed(egui::Key::F5))
            && !self.file_operation_running
            && !self.duplicate_scanning
        {
            self.refresh();
        }
        if ctx.input(|input| input.modifiers.ctrl && input.key_pressed(egui::Key::A))
            && self.preview.is_none()
            && !self.show_duplicates
        {
            self.selected = self
                .display_indices
                .iter()
                .filter_map(|index| self.records.get(*index))
                .map(|record| record.id)
                .collect();
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            if self.show_duplicates {
                if self.duplicate_scanning {
                    self.duplicate_task_id = self.duplicate_service.cancel();
                    self.duplicate_scanning = false;
                    self.status = "已取消重复图片扫描".into();
                }
                self.show_duplicates = false;
            } else if self.preview.is_some() {
                self.close_preview();
            } else {
                self.selected.clear();
            }
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Delete))
            && !self.selected.is_empty()
            && self.pending.is_none()
            && !self.show_duplicates
            && !self.file_operation_running
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
                if let Some(index) = self
                    .preview
                    .filter(|index| *index + 1 < self.display_indices.len())
                {
                    self.open_preview(index + 1);
                }
            }
        }
    }

    fn duplicate_window(&mut self, ctx: &egui::Context) {
        if !self.show_duplicates {
            return;
        }

        const GROUPS_PER_PAGE: usize = 20;
        let mut open = true;
        let mut cancel_scan = false;
        let mut request_rescan = false;
        let mut requested_filter = None;
        let mut keeper_changed = false;
        let mut requested_mode = None;
        let mut used_texture_keys = Vec::new();
        let scanning = self.duplicate_scanning;
        let progress = self.duplicate_stats.clone();
        let deduplicated_view = self.deduplicated_view;
        let textures = &self.textures;
        let failed_images = &self.failed_images;
        let thumbnails = &self.thumbnails;
        let view = &mut self.duplicate_view;

        egui::Window::new(if scanning {
            "正在查找重复图片"
        } else {
            "重复图片清理"
        })
        .open(&mut open)
        .default_size(egui::vec2(900.0, 680.0))
        .min_size(egui::vec2(700.0, 480.0))
        .show(ctx, |ui| {
            if scanning {
                ui.heading("正在比对文件内容…");
                ui.label("只读取大小相同的候选文件，界面仍可继续浏览图片。");
                ui.add_space(8.0);
                let fraction = if progress.candidate_files == 0 {
                    0.0
                } else {
                    progress.checked_files as f32 / progress.candidate_files as f32
                };
                ui.add(
                    egui::ProgressBar::new(fraction.clamp(0.0, 1.0)).text(format!(
                        "已检查 {} / {}",
                        progress.checked_files, progress.candidate_files
                    )),
                );
                ui.label(format!(
                    "读取 {}；复用哈希 {}；新计算 {}；发现 {} 组；错误 {}；耗时 {:.1}s",
                    format_bytes(progress.bytes_read),
                    progress.reused_hashes,
                    progress.hashed_files,
                    progress.duplicate_groups,
                    progress.errors,
                    progress.elapsed_ms as f64 / 1000.0,
                ));
                ui.add_space(8.0);
                if ui.button("取消查重").clicked() {
                    cancel_scan = true;
                }
                return;
            }

            let Some(view) = view.as_mut() else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("正在等待目录校验完成后重新查重…");
                });
                return;
            };

            let total_files = view
                .groups
                .iter()
                .map(|group| group.members.len())
                .sum::<usize>();
            let total_size = view
                .groups
                .iter()
                .map(DuplicateGroup::total_size)
                .sum::<u64>();
            let enabled_groups = view.groups.iter().filter(|group| group.included).count();
            let keep_one_count = view
                .groups
                .iter()
                .filter(|group| group.included)
                .map(|group| group.members.len().saturating_sub(1))
                .sum::<usize>();
            let keep_one_size = view
                .groups
                .iter()
                .filter(|group| group.included)
                .map(DuplicateGroup::reclaimable_size)
                .sum::<u64>();
            let delete_all_count = view
                .groups
                .iter()
                .filter(|group| group.included)
                .map(|group| group.members.len())
                .sum::<usize>();
            let delete_all_size = view
                .groups
                .iter()
                .filter(|group| group.included)
                .map(DuplicateGroup::total_size)
                .sum::<u64>();

            ui.heading(format!("发现 {} 组重复图片", view.groups.len()));
            ui.label(format!(
                "共 {total_files} 张，文件总大小 {}；已启用 {enabled_groups} 组。",
                format_bytes(total_size)
            ));
            ui.label(format!(
                "复用哈希 {}，新计算 {}，读取 {}，耗时 {:.2}s。",
                view.stats.reused_hashes,
                view.stats.hashed_files,
                format_bytes(view.stats.bytes_read),
                view.stats.elapsed_ms as f64 / 1000.0,
            ));
            ui.label("这里只判断文件内容完全相同；改尺寸或重新压缩的图片不会归为重复项。");
            ui.horizontal_wrapped(|ui| {
                if ui.button("重新查重").clicked() {
                    request_rescan = true;
                }
                let mut filter_enabled = deduplicated_view;
                if ui
                    .add_enabled(
                        !view.groups.is_empty(),
                        egui::Checkbox::new(&mut filter_enabled, "主界面中每组重复图片只显示一张"),
                    )
                    .on_hover_text("保留所有非重复图片；这里只过滤显示，不会删除文件。")
                    .on_disabled_hover_text("没有可过滤的重复图片组。")
                    .changed()
                {
                    requested_filter = Some(filter_enabled);
                }
            });

            if !view.errors.is_empty() {
                egui::CollapsingHeader::new(format!(
                    "有 {} 个文件未能检查（点击查看）",
                    view.errors.len()
                ))
                .show(ui, |ui| {
                    for (path, error) in view.errors.iter().take(100) {
                        ui.label(format!("{}：{error}", path.display()));
                    }
                    if view.errors.len() > 100 {
                        ui.label(format!("…以及另外 {} 项", view.errors.len() - 100));
                    }
                });
            }
            ui.separator();

            if view.groups.is_empty() {
                ui.centered_and_justified(|ui| ui.heading("没有找到内容完全相同的图片"));
                return;
            }

            let page_count = view.groups.len().div_ceil(GROUPS_PER_PAGE).max(1);
            view.page = view.page.min(page_count - 1);
            ui.horizontal(|ui| {
                if ui.button("全部启用").clicked() {
                    for group in &mut view.groups {
                        group.included = true;
                    }
                }
                if ui.button("全部排除").clicked() {
                    for group in &mut view.groups {
                        group.included = false;
                    }
                }
                ui.separator();
                if ui
                    .add_enabled(view.page > 0, egui::Button::new("上一页"))
                    .clicked()
                {
                    view.page -= 1;
                }
                ui.label(format!("第 {} / {} 页", view.page + 1, page_count));
                if ui
                    .add_enabled(view.page + 1 < page_count, egui::Button::new("下一页"))
                    .clicked()
                {
                    view.page += 1;
                }
            });

            let start = view.page * GROUPS_PER_PAGE;
            let end = (start + GROUPS_PER_PAGE).min(view.groups.len());
            egui::ScrollArea::vertical()
                .id_salt("duplicate-groups")
                .max_height(430.0)
                .show(ui, |ui| {
                    for (offset, group) in view.groups[start..end].iter_mut().enumerate() {
                        let group_number = start + offset + 1;
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut group.included, "纳入清理");
                                ui.strong(format!(
                                    "第 {group_number} 组：{} 张，每张 {}，可清理 {}",
                                    group.members.len(),
                                    format_bytes(group.members[0].size),
                                    format_bytes(group.reclaimable_size()),
                                ));
                            });
                            egui::CollapsingHeader::new("查看图片并选择保留项")
                                .id_salt(&group.hash)
                                .default_open(group_number == 1)
                                .show(ui, |ui| {
                                    for record in &group.members {
                                        let key = texture_key(record, ImageKind::Thumbnail);
                                        if !textures.contains_key(&key)
                                            && !failed_images.contains(&key)
                                        {
                                            thumbnails.request_thumbnail(
                                                record.clone(),
                                                ThumbnailPriority::Visible,
                                            );
                                        }
                                        used_texture_keys.push(key.clone());
                                        ui.horizontal(|ui| {
                                            if ui
                                                .radio(group.keeper_id == record.id, "保留")
                                                .clicked()
                                            {
                                                group.keeper_id = record.id;
                                                keeper_changed = true;
                                            }
                                            let (rect, _) = ui.allocate_exact_size(
                                                egui::vec2(72.0, 72.0),
                                                egui::Sense::hover(),
                                            );
                                            ui.painter().rect_filled(
                                                rect,
                                                3.0,
                                                ui.visuals().extreme_bg_color,
                                            );
                                            if let Some(texture) = textures.get(&key) {
                                                let natural = texture.size_vec2();
                                                let scale =
                                                    (72.0 / natural.x).min(72.0 / natural.y);
                                                let image_rect = egui::Rect::from_center_size(
                                                    rect.center(),
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
                                            } else {
                                                ui.painter().text(
                                                    rect.center(),
                                                    egui::Align2::CENTER_CENTER,
                                                    if failed_images.contains(&key) {
                                                        "无法预览"
                                                    } else {
                                                        "加载中…"
                                                    },
                                                    egui::FontId::proportional(12.0),
                                                    ui.visuals().weak_text_color(),
                                                );
                                            }
                                            ui.vertical(|ui| {
                                                ui.strong(&record.file_name);
                                                ui.label(&record.relative_path);
                                                ui.label(format!(
                                                    "{}　{}",
                                                    format_bytes(record.size),
                                                    format_modified_time(record.modified_ns),
                                                ));
                                            });
                                        });
                                    }
                                });
                        });
                        ui.add_space(6.0);
                    }
                });

            ui.separator();
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        keep_one_count > 0,
                        egui::Button::new(format!(
                            "删除多余副本（保留 1 张，共 {keep_one_count} 张 / {}）",
                            format_bytes(keep_one_size)
                        )),
                    )
                    .clicked()
                {
                    requested_mode = Some(DuplicateDeleteMode::KeepOne);
                }
                if ui
                    .add_enabled(
                        delete_all_count > 0,
                        egui::Button::new(format!(
                            "删除整个重复组（不保留，共 {delete_all_count} 张 / {}）",
                            format_bytes(delete_all_size)
                        )),
                    )
                    .clicked()
                {
                    requested_mode = Some(DuplicateDeleteMode::DeleteAll);
                }
            });
        });

        for key in used_texture_keys {
            self.touch_texture(&key);
        }
        if cancel_scan || (!open && self.duplicate_scanning) {
            self.duplicate_task_id = self.duplicate_service.cancel();
            self.duplicate_scanning = false;
            self.status = "已取消重复图片扫描".into();
        }
        if !open {
            self.show_duplicates = false;
        }
        if keeper_changed && self.deduplicated_view {
            self.refresh_deduplicated_view();
        }
        if let Some(enabled) = requested_filter {
            self.set_deduplicated_view(enabled);
        }
        if request_rescan {
            self.duplicate_operation_errors.clear();
            self.start_duplicate_scan();
        }
        if let Some(mode) = requested_mode {
            self.pending = Some(PendingDialog::DuplicateDelete(DuplicateDeleteDialog {
                mode,
                permanent_stage: false,
                confirmation_text: String::new(),
            }));
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.show_about {
            let mut open = true;
            let mut close_clicked = false;
            let donation_texture = self.donation_texture(ctx);
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
                            ui.label("微信公众号：");
                            ui.label("大王没有玉玺");
                            ui.end_row();
                            ui.label("GitHub：");
                            ui.hyperlink_to(
                                "github.com/XMQSVIP/TuHaiView",
                                "https://github.com/XMQSVIP/TuHaiView",
                            );
                            ui.end_row();
                            ui.label("版本号：");
                            ui.label(crate::APP_VERSION);
                            ui.end_row();
                        });
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(6.0);
                    ui.vertical_centered(|ui| {
                        ui.strong("微信赞赏码");
                        ui.add_space(6.0);
                        if let Some(texture) = &donation_texture {
                            ui.add(egui::Image::new((texture.id(), egui::vec2(360.0, 360.0))));
                        } else {
                            ui.label("赞赏码加载失败");
                        }
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
            PendingDialog::DuplicateDelete(mut dialog) => {
                let (group_count, file_count, total_size) =
                    self.duplicate_target_summary(dialog.mode);
                let preview_paths = self
                    .duplicate_targets(dialog.mode)
                    .into_iter()
                    .take(8)
                    .map(|(record, _)| record.path)
                    .collect::<Vec<_>>();
                let mut execute_action = None;
                let title = match dialog.mode {
                    DuplicateDeleteMode::KeepOne => "确认删除多余副本",
                    DuplicateDeleteMode::DeleteAll => "确认删除整个重复组",
                };
                egui::Window::new(title)
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        match dialog.mode {
                            DuplicateDeleteMode::KeepOne => {
                                ui.strong("每个重复组将保留你指定的 1 张图片。");
                            }
                            DuplicateDeleteMode::DeleteAll => {
                                ui.colored_label(
                                    egui::Color32::from_rgb(190, 40, 35),
                                    "这些重复组中的所有图片都会删除，不保留任何一张。",
                                );
                            }
                        }
                        ui.label(format!(
                            "将处理 {group_count} 组、{file_count} 张图片，共 {}。",
                            format_bytes(total_size)
                        ));
                        ui.label("执行前会再次核对路径、大小、修改时间和缓存哈希；发生变化的文件会跳过。");
                        for path in &preview_paths {
                            ui.label(path.display().to_string());
                        }
                        if file_count > preview_paths.len() {
                            ui.label(format!("…以及另外 {} 张", file_count - preview_paths.len()));
                        }
                        ui.add_space(8.0);

                        if dialog.permanent_stage {
                            let delete_all = dialog.mode == DuplicateDeleteMode::DeleteAll;
                            if delete_all {
                                ui.colored_label(
                                    egui::Color32::from_rgb(190, 40, 35),
                                    "永久删除整个重复组不可恢复。请输入“永久删除”后才能继续：",
                                );
                                ui.text_edit_singleline(&mut dialog.confirmation_text);
                            } else {
                                ui.colored_label(
                                    egui::Color32::from_rgb(190, 40, 35),
                                    "永久删除不可恢复。请再次确认每组保留项无误。",
                                );
                            }
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(
                                        file_count > 0
                                            && (!delete_all
                                                || dialog.confirmation_text.trim() == "永久删除"),
                                        egui::Button::new("确认永久删除"),
                                    )
                                    .clicked()
                                {
                                    execute_action = Some(FileAction::PermanentDelete);
                                    keep = false;
                                }
                                if ui.button("返回").clicked() {
                                    dialog.permanent_stage = false;
                                    dialog.confirmation_text.clear();
                                }
                                if ui.button("取消").clicked() {
                                    keep = false;
                                }
                            });
                        } else {
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(file_count > 0, egui::Button::new("移入回收站"))
                                    .clicked()
                                {
                                    execute_action = Some(FileAction::RecycleDelete);
                                    keep = false;
                                }
                                if ui
                                    .add_enabled(file_count > 0, egui::Button::new("永久删除…"))
                                    .clicked()
                                {
                                    dialog.permanent_stage = true;
                                }
                                if ui.button("取消").clicked() {
                                    keep = false;
                                }
                            });
                        }
                    });
                if let Some(action) = execute_action {
                    self.submit_duplicate_delete(dialog.mode, action);
                }
                if keep {
                    self.pending = Some(PendingDialog::DuplicateDelete(dialog));
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
                            // 亮黄色在浅色窗口背景上对比度太低，使用深橙红色确保警告清晰可读。
                            egui::Color32::from_rgb(190, 70, 20),
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
                    self.invalidate_duplicates();
                    let database = self.catalog.clear_database();
                    let thumbnails = self.thumbnails.clear_disk_cache();
                    match (database, thumbnails) {
                        (Ok(()), Ok(())) => {
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
                        (database, thumbnails) => {
                            let mut errors = Vec::new();
                            if let Err(error) = database {
                                errors.push(format!("数据库：{error}"));
                            }
                            if let Err(error) = thumbnails {
                                errors.push(format!("缓存：{error}"));
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
        self.duplicate_window(ctx);
        self.dialogs(ctx);
        if self.scanning
            || self.empty_folder_scanning
            || self.duplicate_scanning
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
        self.duplicate_service.cancel();
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

/// 保持缩略图的最小卡片宽度，并把整行余量平均分给所有列。
fn grid_layout(available_width: f32, thumbnail_size: f32, spacing: f32) -> (usize, f32) {
    let minimum_cell_width = thumbnail_size + 20.0;
    let columns = ((available_width + spacing) / (minimum_cell_width + spacing))
        .floor()
        .max(1.0) as usize;
    let total_spacing = spacing * columns.saturating_sub(1) as f32;
    let cell_width = ((available_width - total_spacing) / columns as f32).max(1.0);
    (columns, cell_width)
}

/// 构建主网格的轻量索引。完整记录仍保留给数据库同步和文件操作使用。
fn build_display_indices(
    records: &[ImageRecord],
    groups: Option<&[DuplicateGroup]>,
    deduplicated_view: bool,
) -> Vec<usize> {
    if !deduplicated_view {
        return (0..records.len()).collect();
    }
    let hidden_ids = groups
        .into_iter()
        .flatten()
        .filter(|group| {
            group
                .members
                .iter()
                .any(|record| record.id == group.keeper_id)
        })
        .flat_map(|group| {
            group
                .members
                .iter()
                .filter(|record| record.id != group.keeper_id)
                .map(|record| record.id)
        })
        .collect::<HashSet<_>>();
    records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| (!hidden_ids.contains(&record.id)).then_some(index))
        .collect()
}

fn retain_visible_selection(
    selected: &mut HashSet<i64>,
    records: &[ImageRecord],
    display_indices: &[usize],
) {
    let visible_ids = display_indices
        .iter()
        .filter_map(|index| records.get(*index))
        .map(|record| record.id)
        .collect::<HashSet<_>>();
    selected.retain(|id| visible_ids.contains(id));
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

fn format_modified_time(modified_ns: i64) -> String {
    let seconds = modified_ns.div_euclid(1_000_000_000);
    let nanos = modified_ns.rem_euclid(1_000_000_000) as u32;
    chrono::DateTime::from_timestamp(seconds, nanos)
        .map(|value| {
            value
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "修改时间未知".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: i64) -> ImageRecord {
        ImageRecord {
            id,
            path: PathBuf::from(format!(r"C:\images\{id}.jpg")),
            relative_path: format!("{id}.jpg"),
            file_name: format!("{id}.jpg"),
            size: 10,
            modified_ns: 1,
            width: None,
            height: None,
            format: "jpg".into(),
            thumbnail_key: format!("thumb-{id}"),
            content_hash: Some("a".repeat(64)),
        }
    }

    #[test]
    fn deduplicated_display_keeps_unique_images_and_one_per_group() {
        let records = (1..=6).map(record).collect::<Vec<_>>();
        let groups = vec![
            DuplicateGroup {
                hash: "a".repeat(64),
                members: vec![records[1].clone(), records[2].clone()],
                keeper_id: 2,
                included: false,
            },
            DuplicateGroup {
                hash: "b".repeat(64),
                members: vec![records[3].clone(), records[4].clone()],
                keeper_id: 5,
                included: true,
            },
        ];

        let indices = build_display_indices(&records, Some(&groups), true);
        let ids = indices
            .iter()
            .map(|index| records[*index].id)
            .collect::<Vec<_>>();

        assert_eq!(ids, vec![1, 2, 5, 6]);
    }

    #[test]
    fn changing_keeper_changes_representative_without_changing_count() {
        let records = (1..=3).map(record).collect::<Vec<_>>();
        let mut groups = vec![DuplicateGroup {
            hash: "a".repeat(64),
            members: vec![records[0].clone(), records[1].clone()],
            keeper_id: 1,
            included: true,
        }];
        let first = build_display_indices(&records, Some(&groups), true);
        groups[0].keeper_id = 2;
        let second = build_display_indices(&records, Some(&groups), true);

        assert_eq!(first.len(), second.len());
        assert_eq!(
            first
                .iter()
                .map(|index| records[*index].id)
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(
            second
                .iter()
                .map(|index| records[*index].id)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn enabling_filter_drops_hidden_selection_only() {
        let records = (1..=3).map(record).collect::<Vec<_>>();
        let group = DuplicateGroup {
            hash: "a".repeat(64),
            members: vec![records[0].clone(), records[1].clone()],
            keeper_id: 1,
            included: true,
        };
        let indices = build_display_indices(&records, Some(&[group]), true);
        let mut selected = HashSet::from([1, 2, 3]);
        retain_visible_selection(&mut selected, &records, &indices);

        assert_eq!(selected, HashSet::from([1, 3]));
    }

    #[test]
    fn display_index_handles_fifty_thousand_records() {
        let records = (1..=50_000).map(record).collect::<Vec<_>>();
        let groups = (0..5_000)
            .map(|index| {
                let first = index * 2;
                DuplicateGroup {
                    hash: format!("{index:064x}"),
                    members: vec![records[first].clone(), records[first + 1].clone()],
                    keeper_id: records[first].id,
                    included: true,
                }
            })
            .collect::<Vec<_>>();

        let indices = build_display_indices(&records, Some(&groups), true);
        assert_eq!(indices.len(), 45_000);
    }

    #[test]
    fn grid_layout_distributes_the_full_row_width() {
        let available = 1_234.0;
        let spacing = 8.0;
        let (columns, cell_width) = grid_layout(available, 160.0, spacing);

        assert_eq!(columns, 6);
        let used = cell_width * columns as f32 + spacing * (columns - 1) as f32;
        assert!((used - available).abs() < 0.01);
    }

    #[test]
    fn grid_layout_keeps_one_column_in_a_narrow_view() {
        let (columns, cell_width) = grid_layout(120.0, 160.0, 8.0);

        assert_eq!(columns, 1);
        assert_eq!(cell_width, 120.0);
    }
}
