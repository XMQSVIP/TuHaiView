use crate::{
    catalog::{CatalogEvent, CatalogService, MetadataUpdate},
    duplicates::{DuplicateEvent, DuplicateGroup, DuplicateService, DuplicateStats},
    empty_folders::{EmptyFolderEvent, EmptyFolderService},
    file_ops::{self, FileOperationService},
    models::{
        CatalogSnapshot, ConflictPolicy, EmptyFolderCandidate, FileAction, FileOperationRequest,
        ImageRecord, SortMode,
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
    wakeup_pending: Arc<std::sync::atomic::AtomicBool>,
    duplicate_service: DuplicateService,
    empty_folder_service: EmptyFolderService,
    thumbnails: ThumbnailService,
    file_ops: FileOperationService,
    sort_service: SortService,
    root: Option<PathBuf>,
    generation: u64,
    snapshot: Arc<CatalogSnapshot>,
    pending_snapshot: Option<Arc<CatalogSnapshot>>,
    viewport_signature: Option<(u64, usize, usize, usize, usize, bool)>,
    records: Arc<[Arc<ImageRecord>]>,
    record_positions: Arc<HashMap<PathBuf, usize>>,
    display_indices: Arc<[usize]>,
    display_positions: Arc<HashMap<i64, usize>>,
    textures: HashMap<String, crate::gpu_images::GpuImage>,
    uploads: crate::gpu_images::Uploads,
    deferred_image: Option<crate::thumbnails::ImageResult>,
    texture_lru: lru::LruCache<String, ()>,
    pinned_textures: HashSet<String>,
    image_errors: HashMap<String, String>,
    last_scroll: Instant,
    fast_scroll_until: Instant,
    preview_changed: Instant,
    root_started: Instant,
    first_thumbnail: bool,
    first_screen: bool,
    preview_first_display: bool,
    input_started: Option<Instant>,
    viewport_started: Instant,
    viewport_measured: bool,
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
    grid_anchor: Option<i64>,
    pending_sort_anchor: Option<i64>,
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
    rescan_first: Option<Instant>,
    last_frame: Instant,
    perf_run: Option<crate::performance::UiRun>,
}

impl PreviewerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> anyhow::Result<Self> {
        configure_chinese_font(&cc.egui_ctx);
        let repaint_context = cc.egui_ctx.clone();
        let wakeup_pending = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let signal = wakeup_pending.clone();
        let wakeup: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if !signal.swap(true, std::sync::atomic::Ordering::AcqRel) {
                repaint_context.request_repaint();
            }
        });
        let mut app = Self {
            catalog: CatalogService::new(wakeup.clone())?,
            wakeup_pending,
            duplicate_service: DuplicateService::new(wakeup.clone()),
            empty_folder_service: EmptyFolderService::new(wakeup.clone()),
            thumbnails: ThumbnailService::new(wakeup.clone()),
            file_ops: FileOperationService::new(wakeup.clone()),
            sort_service: SortService::new(wakeup),
            root: None,
            generation: 0,
            snapshot: Arc::new(CatalogSnapshot::default()),
            pending_snapshot: None,
            viewport_signature: None,
            records: Arc::from([]),
            record_positions: Arc::default(),
            display_indices: Arc::from([]),
            display_positions: Arc::default(),
            textures: HashMap::new(),
            uploads: crate::gpu_images::Uploads::new(
                cc.wgpu_render_state
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("DX12 render state unavailable"))?,
            ),
            deferred_image: None,
            texture_lru: lru::LruCache::unbounded(),
            pinned_textures: HashSet::new(),
            image_errors: HashMap::new(),
            last_scroll: Instant::now(),
            fast_scroll_until: Instant::now(),
            preview_changed: Instant::now(),
            root_started: Instant::now(),
            first_thumbnail: false,
            first_screen: false,
            preview_first_display: false,
            input_started: None,
            viewport_started: Instant::now(),
            viewport_measured: false,
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
            grid_anchor: None,
            pending_sort_anchor: None,
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
            rescan_first: None,
            last_frame: Instant::now(),
            perf_run: None,
        };
        if let Some(run) = crate::performance::UiRun::from_environment() {
            app.open_root(run.root.clone());
            app.perf_run = Some(run);
        }
        Ok(app)
    }

    fn choose_root(&mut self) {
        if let Some(path) =
            crate::performance::native_dialog(|| rfd::FileDialog::new().pick_folder())
        {
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
        self.input_started = Some(Instant::now());
        // 切换根目录时重置仅属于旧目录的 UI 状态；后台服务用 generation 过滤迟到事件。
        self.close_auxiliary_windows();
        self.duplicate_rescan_after_catalog = false;
        self.duplicate_operation_errors.clear();
        self.invalidate_duplicates();
        self.empty_folder_generation = self.empty_folder_service.cancel();
        self.empty_folder_scanning = false;
        self.show_empty = false;
        self.empty_folders.clear();
        self.root_started = Instant::now();
        self.first_thumbnail = false;
        self.first_screen = false;
        self.thumbnails.set_root(path.clone());
        self.root = Some(path.clone());
        if let Some(old) = self.pending_snapshot.take() {
            self.catalog.retire(old);
        }
        self.viewport_signature = None;
        self.records = Arc::from([]);
        self.record_positions = Arc::default();
        let old_order = std::mem::replace(&mut self.display_indices, Arc::from([]));
        let old_positions = std::mem::take(&mut self.display_positions);
        crate::sorting::retire_order(old_order, old_positions);
        self.data_revision = self.data_revision.wrapping_add(1);
        self.sorting = false;
        self.selected.clear();
        self.selection_anchor = None;
        self.uploads.clear(&self.thumbnails);
        self.texture_lru.clear();
        self.pinned_textures.clear();
        for (_, texture) in self.textures.drain() {
            self.uploads.retire(texture);
        }
        if let Some(result) = self.deferred_image.take() {
            self.thumbnails.discard(result);
        }
        self.texture_bytes = 0;
        self.failed_images.clear();
        self.image_errors.clear();
        self.preview = None;
        self.preview_origin = None;
        self.grid_scroll_offset = 0.0;
        self.grid_anchor = None;
        self.pending_sort_anchor = None;
        self.pending_grid_scroll_offset = Some(0.0);
        self.pending_grid_focus = None;
        self.prefetch_rows = None;
        self.status = "正在载入缓存并扫描…".into();
        self.scanning = true;
        self.generation = self.catalog.scan(path, self.sort);
        self.thumbnails.set_generation(self.generation);
    }

    fn refresh(&mut self) {
        self.input_started = Some(Instant::now());
        if let Some(root) = self.root.clone() {
            let reopen_duplicates = self.duplicate_rescan_after_catalog;
            self.invalidate_duplicates();
            if reopen_duplicates && !self.show_about && !self.show_empty {
                self.open_auxiliary_window(AuxiliaryWindow::Duplicates);
            }
            self.scanning = true;
            self.status = "正在增量校验…".into();
            self.generation = self.catalog.scan(root, self.sort);
            self.thumbnails.set_generation(self.generation);
        }
    }

    fn invalidate_duplicates(&mut self) {
        let preview_ids = self.preview_record_ids();
        self.duplicate_task_id = self.duplicate_service.cancel();
        self.duplicate_scanning = false;
        if let Some(old) = self.duplicate_view.take() {
            self.catalog.retire_value(old);
        }
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
        if let Some(old) = self.duplicate_view.take() {
            self.catalog.retire_value(old);
        }
        self.open_auxiliary_window(AuxiliaryWindow::Duplicates);
        self.duplicate_stats = DuplicateStats::default();
        self.duplicate_task_id = self
            .duplicate_service
            .scan(self.generation, self.records.clone());
        self.duplicate_scanning = true;
        self.status = "正在查找内容完全相同的图片…".into();
    }

    fn apply_catalog_snapshot(&mut self) {
        let Some(snapshot) = self.catalog.take_snapshot() else {
            return;
        };
        if snapshot.generation != self.generation {
            self.catalog.retire(snapshot);
            return;
        }
        if snapshot.revision == self.snapshot.revision
            && snapshot.generation == self.snapshot.generation
        {
            // Metadata uses the identical record positions and never overwrites an in-flight order.
            self.records = snapshot.records.clone();
            self.record_positions = snapshot.by_path.clone();
            let old = std::mem::replace(&mut self.snapshot, snapshot);
            self.catalog.retire(old);
        } else {
            let same_membership = self.pending_snapshot.as_ref().is_some_and(|old| {
                old.revision == snapshot.revision && old.generation == snapshot.generation
            });
            if let Some(old) = self.pending_snapshot.replace(snapshot) {
                self.catalog.retire(old);
            }
            if !same_membership {
                self.request_sort();
            }
        }
    }

    fn process_events(&mut self, ctx: &egui::Context) {
        let event_start = Instant::now();
        self.apply_catalog_snapshot();
        if let Some(CatalogEvent::Progress {
            generation,
            visited_files,
            supported_images,
            reused,
            inserted,
            updated,
        }) = self.catalog.take_progress()
        {
            if generation == self.generation && self.scanning {
                self.status = format!(
                    "正在扫描：文件 {visited_files}，图片 {supported_images}，复用 {reused}，新增 {inserted}，更新 {updated}"
                );
            }
        }
        while event_start.elapsed() < Duration::from_millis(crate::performance::EVENT_BUDGET_MS) {
            let Ok(event) = self.catalog.rx.try_recv() else {
                break;
            };
            match event {
                CatalogEvent::Started { generation } if generation == self.generation => {
                    self.scanning = true;
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
                    self.apply_catalog_snapshot();
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
                    self.invalidate_duplicates();
                    self.status = "目录发生变化，正在校验相关路径…".into();
                    self.scanning = true;
                }
                CatalogEvent::Cleared { generation } if generation == self.generation => {
                    self.status = "索引已清理，正在重新扫描…".into();
                }
                _ => {}
            }
        }

        while event_start.elapsed() < Duration::from_millis(crate::performance::EVENT_BUDGET_MS) {
            let Ok(event) = self.duplicate_service.rx.try_recv() else {
                break;
            };
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
                    self.catalog.queue_hash_updates(updates);
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

        while event_start.elapsed() < Duration::from_millis(crate::performance::EVENT_BUDGET_MS) {
            let Ok(event) = self.empty_folder_service.rx.try_recv() else {
                break;
            };
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

        while event_start.elapsed() < Duration::from_millis(crate::performance::EVENT_BUDGET_MS) {
            let Ok((action, report)) = self.file_ops.rx.try_recv() else {
                break;
            };
            self.file_operation_running = false;
            let was_duplicate_delete = self.duplicate_delete_running
                && matches!(
                    action,
                    FileAction::RecycleDelete | FileAction::PermanentDelete
                );
            self.duplicate_delete_running = false;
            self.catalog
                .queue_changes(report.affected_paths.iter().cloned());
            if matches!(
                action,
                FileAction::Move | FileAction::RecycleDelete | FileAction::PermanentDelete
            ) {
                self.invalidate_duplicates();
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
                if let Some(old) = self.duplicate_view.take() {
                    self.catalog.retire_value(old);
                }
                if !self.show_about && !self.show_empty {
                    self.open_auxiliary_window(AuxiliaryWindow::Duplicates);
                }
                self.duplicate_rescan_after_catalog = true;
                if report.affected_paths.is_empty() {
                    self.duplicate_rescan_after_catalog = false;
                    self.start_duplicate_scan();
                }
            }
            if let Some(root) = self.root.clone().filter(|_| !report.succeeded.is_empty()) {
                self.scanning = true;
                if report.failed.is_empty() && action == FileAction::Copy && root.exists() {
                    ctx.request_repaint();
                }
            }
        }

        while event_start.elapsed() < Duration::from_millis(crate::performance::EVENT_BUDGET_MS) {
            let Ok(result) = self.sort_service.rx.try_recv() else {
                break;
            };
            if result.generation == self.generation
                && self.sort_service.is_current(&result)
                && result.revision
                    == self
                        .pending_snapshot
                        .as_ref()
                        .map_or(self.data_revision, |s| s.revision)
                && result.mode == self.sort
            {
                let preview_ids = self.preview_record_ids();
                if let Some(snapshot) = self.pending_snapshot.take() {
                    if self.records.is_empty() && !snapshot.records.is_empty() {
                        crate::performance::elapsed("first_records_ms", self.root_started);
                        crate::performance::since_start("startup_first_records_ms");
                    }
                    self.records = snapshot.records.clone();
                    self.record_positions = snapshot.by_path.clone();
                    self.data_revision = snapshot.revision;
                    self.selected.retain(|id| snapshot.by_id.contains_key(id));
                    let old = std::mem::replace(&mut self.snapshot, snapshot);
                    self.catalog.retire(old);
                }
                self.viewport_signature = None;
                let old_order = std::mem::replace(&mut self.display_indices, result.indices);
                let old_positions =
                    std::mem::replace(&mut self.display_positions, result.positions);
                crate::sorting::retire_order(old_order, old_positions);
                if self.deduplicated_view {
                    self.selected
                        .retain(|id| self.display_positions.contains_key(id));
                }
                self.restore_preview_by_ids(preview_ids);
                // 仅恢复用户主动排序的锚点；扫描批次排序不能带动滚动位置。
                if let Some(id) = self.pending_sort_anchor.take() {
                    if self.preview.is_none() {
                        self.pending_grid_focus = self.display_positions.get(&id).copied();
                    }
                }
                self.selection_anchor = None;
                self.sorting = false;
            } else {
                crate::sorting::retire_order(result.indices, result.positions);
            }
        }

        crate::performance::elapsed("ui_events_ms", event_start);
        self.process_images(ctx);

        if !self.catalog.rx.is_empty()
            || !self.duplicate_service.rx.is_empty()
            || !self.empty_folder_service.rx.is_empty()
            || !self.file_ops.rx.is_empty()
            || !self.sort_service.rx.is_empty()
            || !self.thumbnails.rx.is_empty()
        {
            ctx.request_repaint();
        }
        if self.rescan_due.is_some_and(|due| Instant::now() >= due)
            && !self.scanning
            && !self.file_operation_running
        {
            self.rescan_due = None;
            self.rescan_first = None;
            self.refresh();
        }
    }

    fn rebuild_display_indices(&mut self) {
        self.request_sort();
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
            .map(AsRef::as_ref)
    }

    fn shared_display_record(&self, index: usize) -> Option<Arc<ImageRecord>> {
        self.display_indices
            .get(index)
            .and_then(|i| self.records.get(*i))
            .cloned()
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
        let hidden = if self.deduplicated_view {
            self.duplicate_view
                .as_ref()
                .into_iter()
                .flat_map(|v| v.groups.iter())
                .map(|g| (g.members.clone(), g.keeper_id))
                .collect()
        } else {
            Vec::new()
        };
        self.sort_service.submit(
            self.generation,
            self.pending_snapshot
                .as_ref()
                .map_or(self.data_revision, |s| s.revision),
            self.sort,
            self.pending_snapshot
                .as_ref()
                .map_or_else(|| self.records.clone(), |s| s.records.clone()),
            hidden,
        );
        self.previous_sort = self.sort;
        self.sorting = true;
    }

    fn reserve_texture_bytes(&mut self, bytes: usize) -> bool {
        let mut attempts = self.texture_lru.len();
        while self.texture_bytes.saturating_add(bytes) > crate::performance::TEXTURE_BYTES
            && attempts > 0
        {
            attempts -= 1;
            let Some((key, ())) = self.texture_lru.pop_lru() else {
                break;
            };
            if self.pinned_textures.contains(&key) {
                self.texture_lru.put(key, ());
                continue;
            }
            if let Some(texture) = self.textures.remove(&key) {
                self.texture_bytes = self
                    .texture_bytes
                    .saturating_sub(texture.size()[0] * texture.size()[1] * 4);
                self.uploads.retire(texture);
            }
            self.texture_lru.pop(&key);
        }
        self.texture_bytes.saturating_add(bytes) <= crate::performance::TEXTURE_BYTES
    }

    fn insert_texture(&mut self, key: String, texture: crate::gpu_images::GpuImage, bytes: usize) {
        if let Some(old) = self.textures.insert(key.clone(), texture) {
            self.texture_bytes = self
                .texture_bytes
                .saturating_sub(old.size()[0] * old.size()[1] * 4);
            self.uploads.retire(old);
        }
        self.texture_bytes += bytes;
        self.texture_lru.put(key, ());
    }
    fn touch_texture(&mut self, key: &str) {
        let _ = self.texture_lru.get(key);
    }

    fn process_images(&mut self, ctx: &egui::Context) {
        self.uploads.reclaim();
        crate::retirement::record_metrics();
        let started = Instant::now();
        let mut remaining = crate::performance::UPLOAD_BYTES;
        let mut count = 0;
        while count < crate::performance::THUMBNAILS_PER_FRAME
            && remaining > 0
            && started.elapsed() < Duration::from_millis(crate::performance::UPLOAD_BUDGET_MS)
        {
            if !self.uploads.is_pending() {
                // A thumbnail waiting for GPU retirement must not hide a new preview.
                let received = self.thumbnails.preview_rx.try_recv().or_else(|_| {
                    self.deferred_image
                        .take()
                        .map(Ok)
                        .unwrap_or_else(|| self.thumbnails.rx.try_recv())
                });
                let Ok(result) = received else {
                    break;
                };
                if !self.thumbnails.is_current(&result) {
                    self.thumbnails.discard(result);
                    continue;
                }
                let Some(record) = self
                    .record_positions
                    .get(&result.path)
                    .and_then(|i| self.records.get(*i))
                    .filter(|r| {
                        r.id == result.record_id
                            && r.modified_ns == result.modified_ns
                            && r.thumbnail_key == result.source_key
                    })
                else {
                    self.thumbnails.discard(result);
                    continue;
                };
                if let Some(error) = &result.error {
                    self.status = match result.failure {
                        Some(crate::thumbnails::FailureKind::ResourceLimit) => {
                            format!("内存预算不足：{error}")
                        }
                        _ => format!("图片加载失败：{error}"),
                    };
                    self.image_errors
                        .insert(result.texture_key.clone(), self.status.clone());
                    self.failed_images.insert(result.texture_key.clone());
                    self.thumbnails.discard(result);
                    continue;
                }
                if record.width != Some(result.source_width)
                    || record.height != Some(result.source_height)
                {
                    self.catalog.queue_metadata_update(MetadataUpdate {
                        id: result.record_id,
                        path: result.path.clone(),
                        modified_ns: result.modified_ns,
                        thumbnail_key: record.thumbnail_key.clone(),
                        width: result.source_width,
                        height: result.source_height,
                    });
                }
                if !self.reserve_texture_bytes(result.pixels.len()) {
                    self.thumbnails.discard(result);
                    break;
                }
                if let Err(result) = self.uploads.queue(result) {
                    if let Some(old) = self.deferred_image.replace(result) {
                        self.thumbnails.discard(old);
                    }
                    break;
                }
            }
            if let Some((result, texture)) =
                self.uploads
                    .advance(&self.thumbnails, &mut remaining, started)
            {
                self.thumbnails.acknowledge(&result);
                let bytes = result.width * result.height * 4;
                if result.kind == ImageKind::Preview {
                    crate::performance::elapsed("preview_ready_ms", self.preview_changed);
                } else if !self.first_thumbnail {
                    self.first_thumbnail = true;
                    crate::performance::elapsed("first_thumbnail_ms", self.root_started);
                    crate::performance::since_start("startup_first_thumbnail_ms");
                }
                self.insert_texture(result.texture_key.clone(), texture, bytes);
                self.thumbnails.discard(result);
                count += 1;
            } else {
                break;
            }
        }
        if self.uploads.is_pending()
            || self.thumbnails.has_results()
            || self.deferred_image.is_some()
            || self.uploads.needs_reclaim()
        {
            ctx.request_repaint();
        }
        self.thumbnails.record_metrics();
        crate::performance::gauge("texture_allocation_bytes", self.uploads.used_bytes() as f64);
        crate::performance::gauge(
            "texture_bytes",
            (self.texture_bytes + self.uploads.pending_bytes()) as f64,
        );
        crate::performance::sample(
            "upload_bytes",
            (crate::performance::UPLOAD_BYTES - remaining) as f64,
        );
        crate::performance::elapsed("upload_submit_ms", started);
        if let Some(status) = self.thumbnails.cache.status.lock().take() {
            self.status = status;
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
        ui.horizontal(|ui| {
            let current = self.thumbnails.cache.settings.lock().disk_cache_gib;
            let mut selected = current;
            egui::ComboBox::from_id_salt("cache-budget")
                .selected_text(format!("缩略图缓存 {current} GiB"))
                .show_ui(ui, |ui| {
                    for gib in [1, 2, 4, 8, 16] {
                        ui.selectable_value(&mut selected, gib, format!("{gib} GiB"));
                    }
                });
            if selected != current {
                self.thumbnails.cache.set_limit(selected);
            }
        });
        if self.sort != self.previous_sort {
            self.input_started = Some(Instant::now());
            self.pending_sort_anchor = self.grid_anchor.filter(|_| self.preview.is_none());
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
        let Some(destination) =
            crate::performance::native_dialog(|| rfd::FileDialog::new().pick_folder())
        else {
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
            .map(|r| r.as_ref().clone())
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
            duplicate_check: None,
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
            for record in group.members.iter() {
                if mode == DuplicateDeleteMode::KeepOne && record.id == group.keeper_id {
                    continue;
                }
                file_count += 1;
                total_size = total_size.saturating_add(record.size);
            }
        }
        (group_count, file_count, total_size)
    }

    fn submit_duplicate_delete(&mut self, mode: DuplicateDeleteMode, action: FileAction) {
        if self.duplicate_scanning || self.file_operation_running {
            return;
        }
        let groups = self
            .duplicate_view
            .as_ref()
            .into_iter()
            .flat_map(|v| v.groups.iter())
            .filter(|g| g.included)
            .map(|g| {
                (
                    g.members.clone(),
                    g.hash.clone(),
                    (mode == DuplicateDeleteMode::KeepOne).then_some(g.keeper_id),
                )
            })
            .collect();
        let request = FileOperationRequest {
            action,
            sources: Vec::new(),
            destination: None,
            conflict: ConflictPolicy::Skip,
            conflict_overrides: HashMap::new(),
            duplicate_check: Some(crate::models::DuplicateDeleteCheck {
                snapshot: self.snapshot.clone(),
                groups,
            }),
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
            self.grid_anchor = self.display_record(visible.start * columns).map(|r| r.id);
            let visible_count = visible.len().max(1);
            let prefetch_start = visible.start.saturating_sub(visible_count);
            let prefetch_end = (visible.end + visible_count).min(rows);
            let allow_prefetch =
                !self.duplicate_scanning && Instant::now() >= self.fast_scroll_until;
            let signature = (
                self.data_revision,
                visible.start,
                visible.end,
                columns,
                rows,
                allow_prefetch,
            );
            if self.viewport_signature != Some(signature) {
                self.viewport_signature = Some(signature);
                let visible_keys: Vec<_> = (visible.start * columns
                    ..(visible.end * columns).min(self.display_indices.len()))
                    .filter_map(|i| self.display_record(i).map(|r| r.thumbnail_key.clone()))
                    .collect();
                let prefetch_keys: Vec<_> = (prefetch_start * columns
                    ..(prefetch_end * columns).min(self.display_indices.len()))
                    .filter_map(|i| self.display_record(i).map(|r| r.thumbnail_key.clone()))
                    .collect();
                let pinned: HashSet<_> =
                    visible_keys.iter().map(|k| format!("{k}:thumb")).collect();
                if pinned != self.pinned_textures {
                    self.viewport_started = Instant::now();
                    self.viewport_measured = false;
                }
                self.pinned_textures = pinned;
                self.thumbnails
                    .set_viewport(visible_keys, prefetch_keys, allow_prefetch);
            }
            let mut requests = Vec::new();
            for row in visible.clone() {
                for column in 0..columns {
                    let index = row * columns + column;
                    let Some(record) = self.shared_display_record(index) else {
                        break;
                    };
                    let key = texture_key(&record, ImageKind::Thumbnail);
                    if !self.textures.contains_key(&key) && !self.failed_images.contains(&key) {
                        requests.push((record, ThumbnailPriority::Visible));
                    }
                }
            }
            if allow_prefetch {
                // 预取范围保持在视口上下各一屏；范围变化时让旧预取任务失效。
                let prefetch_start = visible.start.saturating_sub(visible_count);
                let prefetch_end = (visible.end + visible_count).min(rows);
                let prefetch_rows = (prefetch_start, prefetch_end);
                if self.prefetch_rows != Some(prefetch_rows) {
                    self.prefetch_rows = Some(prefetch_rows);
                }
                for row in prefetch_start..prefetch_end {
                    if visible.contains(&row) {
                        continue;
                    }
                    for column in 0..columns {
                        let index = row * columns + column;
                        let Some(record) = self.shared_display_record(index) else {
                            break;
                        };
                        let key = texture_key(&record, ImageKind::Thumbnail);
                        if !self.textures.contains_key(&key) && !self.failed_images.contains(&key) {
                            requests.push((record, ThumbnailPriority::Prefetch));
                        }
                    }
                }
            }
            self.thumbnails.request_thumbnails(requests);
            for row in visible {
                ui.horizontal(|ui| {
                    for column in 0..columns {
                        let index = row * columns + column;
                        if index >= self.display_indices.len() {
                            break;
                        }
                        let Some(record) = self.shared_display_record(index) else {
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
        let now = Instant::now();
        let delta = (output.state.offset.y - self.grid_scroll_offset).abs();
        if delta > 0.5 {
            let elapsed = now
                .duration_since(self.last_scroll)
                .as_secs_f32()
                .max(0.001);
            if delta / elapsed > viewport_height * 2.0 {
                self.fast_scroll_until = now + Duration::from_millis(150);
            }
            self.last_scroll = now;
        }
        if now < self.fast_scroll_until {
            ui.ctx().request_repaint_after(self.fast_scroll_until - now);
        }
        self.grid_scroll_offset = output.state.offset.y;
        crate::performance::sample(
            "visible_texture_missing",
            self.pinned_textures
                .iter()
                .filter(|k| !self.textures.contains_key(*k))
                .count() as f64,
        );
        if !self.viewport_measured
            && !self.pinned_textures.is_empty()
            && self
                .pinned_textures
                .iter()
                .all(|k| self.textures.contains_key(k) || self.failed_images.contains(k))
        {
            crate::performance::elapsed("viewport_complete_ms", self.viewport_started);
            self.viewport_measured = true;
            if !self.first_screen
                && self.grid_scroll_offset < 0.5
                && self
                    .pinned_textures
                    .iter()
                    .all(|k| self.textures.contains_key(k))
            {
                crate::performance::elapsed("first_screen_ms", self.root_started);
                crate::performance::since_start("startup_first_screen_ms");
                self.first_screen = true;
            }
        }
    }

    fn open_preview(&mut self, index: usize) {
        self.preview_first_display = false;
        self.input_started = Some(Instant::now());
        if self.preview.is_none() {
            // 记住网格实际滚动偏移，关闭大图预览后回到用户打开的那一行。
            self.preview_origin = Some(index);
            self.preview_return_offset = self.grid_scroll_offset;
        }
        self.preview = Some(index);
        self.preview_changed = Instant::now();
        self.uploads.clear(&self.thumbnails);
        self.thumbnails
            .begin_preview(self.display_record(index).map(|r| r.thumbnail_key.clone()));
        self.pinned_textures.clear();
        if let Some(record) = self.display_record(index) {
            self.pinned_textures
                .insert(texture_key(record, ImageKind::Preview));
        }
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
                self.uploads.retire(texture);
            }
            self.texture_lru.pop(&key);
        }
    }

    fn close_preview(&mut self) {
        self.viewport_signature = None;
        self.thumbnails.begin_preview(None);
        self.uploads.clear(&self.thumbnails);
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
        let Some(record) = self.shared_display_record(index) else {
            self.preview = None;
            return;
        };
        let key = texture_key(&record, ImageKind::Preview);
        self.thumbnails.sync_preview(record.thumbnail_key.clone());
        let viewport = ctx.screen_rect().size() * ctx.pixels_per_point();
        let desired = viewport.x.max(viewport.y)
            * if self.fit_preview {
                1.0
            } else {
                self.zoom.max(1.0)
            };
        let target = if desired <= 1024.0 {
            1024
        } else if desired <= 2048.0 {
            2048
        } else {
            4096
        };
        let loaded = self.textures.get(&key).map_or(0, |t| t.requested_side());
        if loaded < target && !self.failed_images.contains(&key) {
            if loaded == 0 || self.preview_changed.elapsed() >= Duration::from_millis(120) {
                self.thumbnails.request_preview(record.clone(), target);
            } else {
                ctx.request_repaint_after(Duration::from_millis(120));
            }
        }
        let thumbnail_key = texture_key(&record, ImageKind::Thumbnail);
        self.pinned_textures.clear();
        self.pinned_textures.insert(key.clone());
        self.pinned_textures.insert(thumbnail_key.clone());
        self.touch_texture(&key);

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::from_rgb(18, 18, 20)))
            .show(ctx, |ui| {
                *ui.visuals_mut() = egui::Visuals::dark();
                ui.horizontal(|ui| {
                    if ui.button("← 上一张").clicked() && index > 0 {
                        self.open_preview(index - 1);
                    }
                    if ui.button("下一张 →").clicked() && index + 1 < self.display_indices.len()
                    {
                        self.open_preview(index + 1);
                    }
                    if let (Some(w), Some(h)) = (record.width, record.height) {
                        ui.label(format!("原图 {w}×{h}"));
                    }
                    if let Some(texture) = self.textures.get(&key) {
                        ui.label(format!("预览 {}×{}", texture.size()[0], texture.size()[1]));
                    } else if let Some(texture) = self.textures.get(&thumbnail_key) {
                        ui.label(format!(
                            "缩略图 {}×{} · 正在加载预览",
                            texture.size()[0],
                            texture.size()[1]
                        ));
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
                    if ui.button("预览 100%").clicked() {
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
                if let Some(error) = self.image_errors.get(&key) {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                }
                egui::ScrollArea::both()
                    .drag_to_scroll(true)
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        ui.centered_and_justified(|ui| {
                            if let Some(texture) = self
                                .textures
                                .get(&key)
                                .or_else(|| self.textures.get(&thumbnail_key))
                            {
                                if !self.preview_first_display {
                                    crate::performance::elapsed(
                                        "preview_first_display_ms",
                                        self.preview_changed,
                                    );
                                    self.preview_first_display = true;
                                }
                                let available = ui.available_size();
                                let natural = texture.size_vec2() / ctx.pixels_per_point();
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
                                    self.image_errors
                                        .get(&key)
                                        .map(String::as_str)
                                        .unwrap_or("图片无法加载。"),
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
            self.preview_changed = Instant::now();
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
                                    for record in group.members.iter() {
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
                        duplicate_check: None,
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
                    .duplicate_view
                    .as_ref()
                    .into_iter()
                    .flat_map(|v| v.groups.iter())
                    .filter(|g| g.included)
                    .flat_map(|g| {
                        g.members.iter().filter(move |r| {
                            dialog.mode == DuplicateDeleteMode::DeleteAll || r.id != g.keeper_id
                        })
                    })
                    .take(8)
                    .map(|record| record.path.clone())
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
                            self.records = Arc::from([]);
                            self.record_positions = Arc::default();
                            self.data_revision = self.data_revision.wrapping_add(1);
                            self.selected.clear();
                            self.selection_anchor = None;
                            self.uploads.clear(&self.thumbnails);
                            self.texture_lru.clear();
                            self.pinned_textures.clear();
                            for (_, texture) in self.textures.drain() {
                                self.uploads.retire(texture);
                            }
                            if let Some(result) = self.deferred_image.take() {
                                self.thumbnails.discard(result);
                            }
                            self.texture_bytes = 0;
                            self.failed_images.clear();
                            self.image_errors.clear();
                            self.refresh();
                            self.status = "已提交缓存和数据库清理，正在等待后台完成…".into();
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
        self.wakeup_pending
            .store(false, std::sync::atomic::Ordering::Release);
        let frame_start = Instant::now();
        let dialog_time_before = crate::performance::native_dialog_time();
        let actual_input = ctx.input(|i| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key { .. }
                        | egui::Event::PointerButton { .. }
                        | egui::Event::MouseWheel { .. }
                )
            })
        });
        if let Some(run) = self.perf_run.as_mut() {
            if run.route_started.is_none() && !self.scanning && !self.sorting && self.first_screen {
                run.route_started = Some(Instant::now());
                crate::performance::sample("route_ready", 1.0);
            }
        }
        crate::performance::begin_frame(self.perf_run.as_ref().map_or(8, |r| r.phase()));
        let interval = frame_start.duration_since(self.last_frame);
        crate::performance::sample("frame_interval_ms", interval.as_secs_f64() * 1000.0);
        self.last_frame = frame_start;
        self.process_events(ctx);
        if let Some(mut run) = self.perf_run.take() {
            ctx.input(|input| {
                for event in &input.events {
                    if let egui::Event::Screenshot {
                        image, user_data, ..
                    } = event
                    {
                        let name = user_data
                            .data
                            .as_ref()
                            .and_then(|v| v.downcast_ref::<String>())
                            .cloned()
                            .unwrap_or_else(|| "render".into());
                        crate::performance::save_capture(image.clone(), name);
                    }
                }
            });
            let elapsed = run.started.elapsed().as_secs_f64();
            if elapsed >= run.seconds {
                crate::performance::sample("soak_completed_seconds", elapsed);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                // Explicit opt-in Release QA trajectory. Uses the same grid,
                // preview, sorter and uploader as interactive browsing.
                let phase = run.phase();
                if std::env::var("TUHAI_PERF_CAPTURE").ok().as_deref() == Some("1") {
                    for (bit, second, name) in
                        [(1, 8.0, "grid"), (2, 25.0, "preview"), (4, 45.0, "scroll")]
                    {
                        if elapsed >= second && run.captures & bit == 0 {
                            run.captures |= bit;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                                egui::UserData::new(name.to_owned()),
                            ));
                        }
                    }
                }
                crate::performance::sample("trajectory_phase", phase as f64);
                if !self.records.is_empty() && !self.scanning && phase < 6 {
                    if phase == 2 || phase == 3 {
                        let index = ((elapsed * if phase == 2 { 2.0 } else { 10.0 }) as usize)
                            % self.display_indices.len().max(1);
                        if self.preview != Some(index) {
                            self.open_preview(index);
                        }
                    } else {
                        if self.preview.is_some() {
                            self.close_preview();
                        }
                        let speed = if phase == 4 { 2500.0 } else { 600.0 };
                        let t = if run.scenario == "scroll" {
                            run.route_started.map_or(0.0, |s| s.elapsed().as_secs_f64())
                        } else {
                            elapsed
                        } % 20.0;
                        self.pending_grid_scroll_offset =
                            Some((if t < 10.0 { t } else { 20.0 - t }) as f32 * speed);
                        if phase == 5 && run.last_sort != elapsed as u64 {
                            run.last_sort = elapsed as u64;
                            self.sort = if run.last_sort % 2 == 0 {
                                SortMode::NameNatural
                            } else {
                                SortMode::ModifiedDesc
                            };
                            self.request_sort();
                        }
                    }
                }
                if run.scenario == "soak"
                    && phase < 6
                    && elapsed as u64 / 120 > run.last_root_switch
                {
                    run.last_root_switch = elapsed as u64 / 120;
                    if let Some(root) = run.alternate_root.as_ref() {
                        let target = if run.last_root_switch % 2 == 0 {
                            &run.root
                        } else {
                            root
                        };
                        self.open_root(target.clone());
                    }
                }
                if phase < 6 {
                    if let Ok(ms) = std::env::var("TUHAI_PERF_REPAINT_MS")
                        .unwrap_or_default()
                        .parse::<u64>()
                    {
                        // egui subtracts predicted_dt from nonzero repaint delays.
                        let predicted =
                            ctx.input(|i| Duration::from_secs_f32(i.predicted_dt.max(0.0)));
                        ctx.request_repaint_after(Duration::from_millis(ms) + predicted);
                    } else {
                        ctx.request_repaint();
                    }
                } else {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
            }
            self.perf_run = Some(run);
        }
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
        if actual_input {
            // egui events do not carry OS input timestamps: this excludes event-queue wait.
            let wall = frame_start.elapsed();
            let dialog_wait =
                crate::performance::native_dialog_time().saturating_sub(dialog_time_before);
            crate::performance::sample("input_frame_wall_ms", wall.as_secs_f64() * 1000.0);
            crate::performance::sample(
                "input_frame_processing_ms",
                wall.saturating_sub(dialog_wait).as_secs_f64() * 1000.0,
            );
        }
        if let Some(start) = self.input_started.take() {
            crate::performance::elapsed("action_ui_feedback_ms", start);
        }
        crate::performance::sample("grid_scroll_offset", self.grid_scroll_offset as f64);
        crate::performance::gauge("pixels_per_point", ctx.pixels_per_point() as f64);
        if crate::performance::enabled() {
            crate::performance::gauge("egui_data_entries", ctx.data(|d| d.len()) as f64);
            crate::performance::gauge(
                "egui_text_layouts",
                ctx.fonts(|f| f.num_galleys_in_cache()) as f64,
            );
        }
        crate::performance::gauge("catalog_displayed_records", self.records.len() as f64);
        crate::performance::gauge(
            "catalog_displayed_table_estimated_bytes",
            self.snapshot.table_bytes() as f64,
        );
        crate::performance::gauge(
            "catalog_pending_table_estimated_bytes",
            self.pending_snapshot
                .as_ref()
                .map_or(0, |s| s.table_bytes()) as f64,
        );
        crate::performance::gauge(
            "catalog_pending_records",
            self.pending_snapshot
                .as_ref()
                .map_or(0, |s| s.records.len()) as f64,
        );
        crate::performance::gauge("sort_displayed_entries", self.display_indices.len() as f64);
        crate::performance::gauge("texture_displayed_count", self.textures.len() as f64);
        crate::performance::gauge(
            "deferred_pixel_bytes",
            self.deferred_image.as_ref().map_or(0, |r| r.pixels.len()) as f64,
        );
        ctx.input(|i| {
            crate::performance::gauge("window_focused", i.focused as u8 as f64);
            crate::performance::sample(
                "window_minimized",
                i.viewport().minimized.unwrap_or(false) as u8 as f64,
            );
            crate::performance::gauge("window_width", i.screen_rect().width() as f64);
            crate::performance::gauge("window_height", i.screen_rect().height() as f64);
        });
        crate::performance::elapsed("ui_update_ms", frame_start);
        if let Some(seconds) = _frame.info().cpu_usage {
            crate::performance::sample("eframe_cpu_ms", seconds as f64 * 1000.0);
        }
    }

    fn on_exit(&mut self) {
        self.catalog.cancel_scan();
        self.duplicate_service.cancel();
        self.empty_folder_service.cancel();
        self.uploads.clear(&self.thumbnails);
        self.texture_lru.clear();
        self.pinned_textures.clear();
        for (_, texture) in self.textures.drain() {
            self.uploads.retire(texture);
        }
        if let Some(result) = self.deferred_image.take() {
            self.thumbnails.discard(result);
        }
        self.texture_bytes = 0;
        crate::performance::flush_at_exit();
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
#[cfg(test)]
fn build_display_indices<R: AsRef<ImageRecord>>(
    records: &[R],
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
        .filter_map(|(index, record)| (!hidden_ids.contains(&record.as_ref().id)).then_some(index))
        .collect()
}

#[cfg(test)]
fn retain_visible_selection<R: AsRef<ImageRecord>>(
    selected: &mut HashSet<i64>,
    records: &[R],
    display_indices: &[usize],
) {
    let visible_ids = display_indices
        .iter()
        .filter_map(|index| records.get(*index))
        .map(|record| record.as_ref().id)
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
                members: vec![records[1].clone(), records[2].clone()].into(),
                keeper_id: 2,
                included: false,
            },
            DuplicateGroup {
                hash: "b".repeat(64),
                members: vec![records[3].clone(), records[4].clone()].into(),
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
            members: vec![records[0].clone(), records[1].clone()].into(),
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
            members: vec![records[0].clone(), records[1].clone()].into(),
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
                    members: vec![records[first].clone(), records[first + 1].clone()].into(),
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
