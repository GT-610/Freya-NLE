//! Export window commands and production export runtime coordination.
use crate::batch::{self, BatchOutcome, BatchQueue};
use crate::dialogs::show_and_redraw;
use crate::localization::{UiLanguage, current_ui_language, localized};
use crate::playback::PreviewSourceKey;
use crate::projection::{setting_string, sync_transport_weak};
use crate::{ExportWindow, MainWindow, TimelineWindow};
use freya_nle_app::effect_catalog::EffectCatalog;
use freya_nle_app::settings::SettingsStore;
use freya_nle_app::{ApplicationModel, WorkspaceModel};
use freya_nle_export::{
    ExportEvent, ExportImageFormat, ExportManager, ExportMode, ExportProject, ExportRequest,
    ExportRuntimeSettings, available_audio_encoders, available_video_encoders, is_software_codec,
    nearest_audio_bitrate, valid_export_path,
};
use freya_nle_preview::{PlannedPreview, PreviewPlanner};
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

pub(super) const VIDEO_CODECS: [(&str, &str); 16] = [
    ("H.264 – libx264 (SW)", "libx264"),
    ("H.264 – NVENC (NVIDIA)", "h264_nvenc"),
    ("H.264 – AMF (AMD)", "h264_amf"),
    ("H.264 – QSV (Intel)", "h264_qsv"),
    ("H.264 – VAAPI (Linux)", "h264_vaapi"),
    ("H.264 – VideoToolbox (Apple)", "h264_videotoolbox"),
    ("HEVC – libx265 (SW)", "libx265"),
    ("HEVC – NVENC (NVIDIA)", "hevc_nvenc"),
    ("HEVC – AMF (AMD)", "hevc_amf"),
    ("HEVC – QSV (Intel)", "hevc_qsv"),
    ("HEVC – VAAPI (Linux)", "hevc_vaapi"),
    ("HEVC – VideoToolbox (Apple)", "hevc_videotoolbox"),
    ("AV1 – libaom (SW)", "libaom-av1"),
    ("AV1 – NVENC (NVIDIA)", "av1_nvenc"),
    ("AV1 – AMF (AMD)", "av1_amf"),
    ("AV1 – VAAPI (Linux)", "av1_vaapi"),
];

pub(super) const AUDIO_CODECS: [(&str, &str); 5] = [
    ("AAC", "aac"),
    ("Opus", "libopus"),
    ("MP3", "libmp3lame"),
    ("FLAC (Lossless)", "flac"),
    ("PCM 16-bit", "pcm_s16le"),
];

pub(super) const PRESET_VALUES: [&str; 5] = ["ultrafast", "fast", "medium", "slow", "veryslow"];

pub(super) const PROFILE_VALUES: [&str; 4] = ["", "baseline", "main", "high"];

pub(super) const AUDIO_BITRATES: [i32; 5] = [96, 128, 192, 256, 320];

#[derive(Default)]
pub(super) struct ExportCodecState {
    pub(super) video_values: Vec<String>,
    pub(super) audio_values: Vec<String>,
}

#[derive(Default)]
pub(super) struct ExportPlannerRuntime {
    pub(super) planner: Option<PreviewPlanner>,
    pub(super) source_key: Option<PreviewSourceKey>,
}

impl ExportPlannerRuntime {
    pub(super) fn reset(&mut self) {
        self.planner = None;
        self.source_key = None;
    }

    pub(super) fn build(
        &mut self,
        project_instance_id: u64,
        workspace: &WorkspaceModel,
        frame: i32,
        effect_catalog: &EffectCatalog,
    ) -> Option<PlannedPreview> {
        let source_key = PreviewSourceKey {
            project_instance_id,
            document_revision: workspace.document_revision(),
            project_path: workspace.project().path.clone(),
        };
        if self.source_key.as_ref() != Some(&source_key) {
            if let Some(planner) = self.planner.as_mut() {
                planner.rebuild(workspace.document(), workspace.project().path.as_deref());
                planner.set_native_definitions(effect_catalog.native_definitions());
            } else {
                let mut planner =
                    PreviewPlanner::new(workspace.document(), workspace.project().path.as_deref());
                planner.set_native_definitions(effect_catalog.native_definitions());
                self.planner = Some(planner);
            }
            self.source_key = Some(source_key);
        }
        self.planner.as_mut().and_then(|planner| {
            planner.build(workspace.document(), workspace.selected_scene(), frame)
        })
    }
}

pub(super) fn initialize_export_draft(window: &ExportWindow, settings: &SettingsStore) {
    window.set_format_index(0);
    window.set_image_format_index(0);
    window.set_crf_mode(true);
    window.set_bitrate_mode(false);
    window.set_crf(settings.i32_value("exportDefaultCrf", 20).clamp(0, 51) as f32);
    window.set_bitrate_mbps(
        settings
            .i32_value("exportDefaultBitrateMbps", 15)
            .clamp(1, 500),
    );
    window.set_preset_index(2);
    window.set_profile_index(0);
    let audio_bitrate =
        nearest_audio_bitrate(settings.i32_value("exportDefaultAudioBitrateKbps", 192));
    window.set_audio_bitrate_index(
        AUDIO_BITRATES
            .iter()
            .position(|candidate| *candidate == audio_bitrate)
            .unwrap_or(2) as i32,
    );
    window.set_full_range(true);
    window.set_start_frame(0);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn install_export_callbacks(
    main: &MainWindow,
    export: &ExportWindow,
    model: Rc<RefCell<ApplicationModel>>,
    settings: Rc<RefCell<SettingsStore>>,
    manager: Rc<RefCell<ExportManager>>,
    codecs: Rc<RefCell<ExportCodecState>>,
    planner: Rc<RefCell<ExportPlannerRuntime>>,
    batch: Rc<RefCell<BatchQueue>>,
) {
    let open_window = export.as_weak();
    let open_model = model.clone();
    let open_settings = settings.clone();
    let open_manager = manager.clone();
    let open_codecs = codecs.clone();
    let open_batch = batch.clone();
    main.on_export_media(move || {
        let Some(window) = open_window.upgrade() else {
            return;
        };
        if !open_manager.borrow().is_exporting() {
            let application = open_model.borrow();
            let Some(workspace) = application.current_workspace() else {
                return;
            };
            sync_export_window(
                &window,
                workspace,
                &open_settings.borrow(),
                &mut open_codecs.borrow_mut(),
            );
        }
        window.set_batch_completion_index(
            open_settings
                .borrow()
                .i32_value("exportBatchCompletion", 0)
                .clamp(0, 1),
        );
        sync_batch(&window, &open_batch.borrow());
        let _ = show_and_redraw(&window);
    });

    export.on_validate_output_path(|path| valid_export_path(&path));

    let browse_window = export.as_weak();
    export.on_browse_output(move |choose_folder| {
        let Some(window) = browse_window.upgrade() else {
            return;
        };
        if let Some(path) = choose_export_output(choose_folder, &window.get_output_path()) {
            window.set_output_path(path.to_string_lossy().into_owned().into());
        }
    });

    let video_window = export.as_weak();
    let video_codecs = codecs.clone();
    export.on_video_codec_selected(move |index| {
        let Some(window) = video_window.upgrade() else {
            return;
        };
        let codecs = video_codecs.borrow();
        let software = is_software_codec(selected_codec(&codecs.video_values, index, "libx264"));
        window.set_software_codec(software);
    });

    let audio_window = export.as_weak();
    let audio_codecs = codecs.clone();
    export.on_audio_codec_selected(move |index| {
        let Some(window) = audio_window.upgrade() else {
            return;
        };
        let codecs = audio_codecs.borrow();
        let codec = selected_codec(&codecs.audio_values, index, "aac");
        window.set_audio_bitrate_enabled(codec != "flac" && codec != "pcm_s16le");
    });

    let start_window = export.as_weak();
    let start_model = model.clone();
    let start_settings = settings.clone();
    let start_manager = manager.clone();
    let start_codecs = codecs.clone();
    let start_planner = planner.clone();
    export.on_start_export(move || {
        let Some(window) = start_window.upgrade() else {
            return;
        };
        let Some((project_instance_id, project)) = current_export_project(&start_model.borrow())
        else {
            show_export_result(
                &window,
                false,
                localized(
                    "There is no project to export.",
                    "没有可导出的项目。",
                    "書き出すプロジェクトがありません",
                ),
            );
            return;
        };
        let request = match export_request(&window, &start_codecs.borrow()) {
            Ok(request) => request,
            Err(message) => {
                show_export_result(&window, false, &message);
                return;
            }
        };
        let runtime_settings = export_runtime_settings(&start_settings.borrow());
        match start_manager.borrow_mut().start(
            project_instance_id,
            project,
            request,
            runtime_settings,
        ) {
            Ok(total_frames) => {
                start_planner.borrow_mut().reset();
                if let Some(workspace) = start_model.borrow_mut().current_workspace_mut() {
                    workspace.pause_playback();
                }
                window.set_exporting(true);
                window.set_export_progress(0.0);
                window.set_progress_label(
                    format!(
                        "0 / {total_frames} {}",
                        localized("frames", "帧", "フレーム")
                    )
                    .into(),
                );
                window.set_cancel_confirmation_visible(false);
                window.set_result_visible(false);
            }
            Err(error) => show_export_result(&window, false, &error.to_string()),
        }
    });

    let close_window = export.as_weak();
    let close_manager = manager.clone();
    export.on_close_window(move || {
        let Some(window) = close_window.upgrade() else {
            return;
        };
        if close_manager.borrow().is_exporting() {
            window.set_cancel_confirmation_visible(true);
        } else {
            let _ = window.hide();
        }
    });

    let native_close_window = export.as_weak();
    let native_close_manager = manager.clone();
    export.window().on_close_requested(move || {
        let Some(window) = native_close_window.upgrade() else {
            return CloseRequestResponse::HideWindow;
        };
        if native_close_manager.borrow().is_exporting() {
            window.set_cancel_confirmation_visible(true);
            CloseRequestResponse::KeepWindowShown
        } else {
            CloseRequestResponse::HideWindow
        }
    });

    let cancel_window = export.as_weak();
    export.on_request_cancel(move || {
        if let Some(window) = cancel_window.upgrade() {
            window.set_cancel_confirmation_visible(true);
        }
    });

    let confirm_window = export.as_weak();
    let confirm_manager = manager.clone();
    let confirm_batch = batch.clone();
    export.on_confirm_cancel(move || {
        confirm_manager.borrow_mut().cancel();
        // Cancelling the item must also stop the queue; otherwise the finish
        // event would record a failure and start the next item.
        confirm_batch.borrow_mut().stop();
        confirm_batch.borrow_mut().set_message(localized(
            "Batch stopped",
            "批次已停止",
            "バッチを停止しました",
        ));
        if let Some(window) = confirm_window.upgrade() {
            window.set_cancel_confirmation_visible(false);
            sync_batch(&window, &confirm_batch.borrow());
        }
    });

    let dismiss_cancel_window = export.as_weak();
    export.on_dismiss_cancel(move || {
        if let Some(window) = dismiss_cancel_window.upgrade() {
            window.set_cancel_confirmation_visible(false);
        }
    });

    let dismiss_result_window = export.as_weak();
    export.on_dismiss_result(move || {
        if let Some(window) = dismiss_result_window.upgrade() {
            window.set_result_visible(false);
        }
    });

    let batch_add_window = export.as_weak();
    let batch_add_codecs = codecs.clone();
    let batch_add_queue = batch.clone();
    let batch_add_settings = settings.clone();
    export.on_batch_add(move || {
        let Some(window) = batch_add_window.upgrade() else {
            return;
        };
        let request = match export_request(&window, &batch_add_codecs.borrow()) {
            Ok(request) => request,
            Err(message) => {
                batch_add_queue.borrow_mut().set_message(message);
                sync_batch(&window, &batch_add_queue.borrow());
                return;
            }
        };
        let label = request
            .output_path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut queue = batch_add_queue.borrow_mut();
        if queue.push(label.clone(), request).is_none() {
            queue.set_message(localized(
                "The batch is full",
                "批次已满",
                "バッチがいっぱいです",
            ));
        } else {
            queue.set_message(format!(
                "{} {label}",
                localized("Added", "已加入", "追加しました")
            ));
            let last = queue.items().len() as i32 - 1;
            window.set_batch_current(last);
        }
        sync_batch(&window, &queue);
        drop(queue);
        persist_batch(
            &batch_add_settings,
            &batch_add_queue,
            window.get_batch_completion_index(),
        );
    });

    let batch_move_window = export.as_weak();
    let batch_move_queue = batch.clone();
    let batch_move_settings = settings.clone();
    export.on_batch_move(move |delta| {
        let Some(window) = batch_move_window.upgrade() else {
            return;
        };
        let current = window.get_batch_current().max(0) as usize;
        let moved = batch_move_queue.borrow_mut().move_by(current, delta);
        if let Some(index) = moved {
            window.set_batch_current(index as i32);
        }
        sync_batch(&window, &batch_move_queue.borrow());
        persist_batch(
            &batch_move_settings,
            &batch_move_queue,
            window.get_batch_completion_index(),
        );
    });

    let batch_remove_window = export.as_weak();
    let batch_remove_queue = batch.clone();
    let batch_remove_settings = settings.clone();
    export.on_batch_remove(move || {
        let Some(window) = batch_remove_window.upgrade() else {
            return;
        };
        let current = window.get_batch_current().max(0) as usize;
        if batch_remove_queue.borrow_mut().remove(current) {
            window.set_batch_current(current.saturating_sub(1) as i32);
        }
        sync_batch(&window, &batch_remove_queue.borrow());
        persist_batch(
            &batch_remove_settings,
            &batch_remove_queue,
            window.get_batch_completion_index(),
        );
    });

    let batch_clear_window = export.as_weak();
    let batch_clear_queue = batch.clone();
    let batch_clear_settings = settings.clone();
    export.on_batch_clear(move || {
        let Some(window) = batch_clear_window.upgrade() else {
            return;
        };
        batch_clear_queue.borrow_mut().clear();
        window.set_batch_current(-1);
        sync_batch(&window, &batch_clear_queue.borrow());
        persist_batch(
            &batch_clear_settings,
            &batch_clear_queue,
            window.get_batch_completion_index(),
        );
    });

    let batch_start_window = export.as_weak();
    let batch_start_model = model.clone();
    let batch_start_manager = manager.clone();
    let batch_start_settings = settings.clone();
    let batch_start_planner = planner.clone();
    let batch_start_queue = batch.clone();
    export.on_batch_start(move || {
        let Some(window) = batch_start_window.upgrade() else {
            return;
        };
        let empty = batch_start_queue.borrow().is_empty();
        if empty {
            batch_start_queue.borrow_mut().set_message(localized(
                "The batch is empty",
                "批次为空",
                "バッチが空です",
            ));
            sync_batch(&window, &batch_start_queue.borrow());
            return;
        }
        if batch_start_manager.borrow().is_exporting() {
            batch_start_queue.borrow_mut().set_message(localized(
                "Another export is already running",
                "已有一个导出正在运行",
                "書き出しが実行中です",
            ));
            sync_batch(&window, &batch_start_queue.borrow());
            return;
        }
        begin_batch_item(
            &window,
            &batch_start_model,
            &batch_start_manager,
            &batch_start_settings,
            &batch_start_planner,
            &batch_start_queue,
            0,
        );
    });

    let batch_stop_window = export.as_weak();
    let batch_stop_manager = manager.clone();
    let batch_stop_queue = batch.clone();
    export.on_batch_stop(move || {
        let Some(window) = batch_stop_window.upgrade() else {
            return;
        };
        batch_stop_queue.borrow_mut().stop();
        {
            let mut queue = batch_stop_queue.borrow_mut();
            queue.set_message(localized(
                "Batch stopped",
                "批次已停止",
                "バッチを停止しました",
            ));
        }
        if batch_stop_manager.borrow().is_exporting() {
            batch_stop_manager.borrow_mut().cancel();
        }
        window.set_exporting(false);
        window.set_cancel_confirmation_visible(false);
        sync_batch(&window, &batch_stop_queue.borrow());
    });

    let batch_pause_window = export.as_weak();
    let batch_pause_queue = batch.clone();
    export.on_batch_pause(move || {
        let Some(window) = batch_pause_window.upgrade() else {
            return;
        };
        let mut queue = batch_pause_queue.borrow_mut();
        queue.request_pause();
        queue.set_message(localized(
            "Pausing after this item",
            "当前项完成后暂停",
            "この項目の後に一時停止します",
        ));
        sync_batch(&window, &queue);
    });

    let batch_resume_window = export.as_weak();
    let batch_resume_model = model.clone();
    let batch_resume_manager = manager.clone();
    let batch_resume_settings = settings.clone();
    let batch_resume_planner = planner.clone();
    let batch_resume_queue = batch.clone();
    export.on_batch_resume(move || {
        let Some(window) = batch_resume_window.upgrade() else {
            return;
        };
        let next = batch_resume_queue.borrow_mut().resume();
        match next {
            Some(index) => begin_batch_item(
                &window,
                &batch_resume_model,
                &batch_resume_manager,
                &batch_resume_settings,
                &batch_resume_planner,
                &batch_resume_queue,
                index,
            ),
            None => sync_batch(&window, &batch_resume_queue.borrow()),
        }
    });

    let batch_completion_window = export.as_weak();
    let batch_completion_settings = settings.clone();
    let batch_completion_queue = batch.clone();
    export.on_batch_completion_changed(move || {
        let Some(window) = batch_completion_window.upgrade() else {
            return;
        };
        persist_batch(
            &batch_completion_settings,
            &batch_completion_queue,
            window.get_batch_completion_index(),
        );
    });
}

/// Mirrors the queue into the export window.
fn sync_batch(window: &ExportWindow, queue: &BatchQueue) {
    window.set_batch_items(ModelRc::new(VecModel::from(queue.rows())));
    window.set_batch_running(queue.is_running());
    window.set_batch_paused(queue.is_paused());
    window.set_batch_status(queue.message().into());
    let current = window.get_batch_current();
    if current >= queue.items().len() as i32 {
        window.set_batch_current(queue.items().len() as i32 - 1);
    }
}

/// Writes the queue and the completion choice into the settings file.
fn persist_batch(
    settings: &Rc<RefCell<SettingsStore>>,
    queue: &Rc<RefCell<BatchQueue>>,
    completion_index: i32,
) {
    let mut snapshot = settings.borrow().snapshot();
    for (key, value) in batch::settings_entry(&queue.borrow(), completion_index) {
        snapshot.insert(key, value);
    }
    let _ = settings.borrow_mut().apply(snapshot);
}

/// Starts one queued item, recording a failure instead of aborting the batch.
fn begin_batch_item(
    window: &ExportWindow,
    model: &Rc<RefCell<ApplicationModel>>,
    manager: &Rc<RefCell<ExportManager>>,
    settings: &Rc<RefCell<SettingsStore>>,
    planner: &Rc<RefCell<ExportPlannerRuntime>>,
    batch: &Rc<RefCell<BatchQueue>>,
    index: usize,
) {
    let Some((project_instance_id, project)) = current_export_project(&model.borrow()) else {
        let mut queue = batch.borrow_mut();
        queue.begin(index);
        queue.finish_running(BatchOutcome::Failed(
            localized(
                "There is no project to export.",
                "没有可导出的项目。",
                "書き出すプロジェクトがありません",
            )
            .to_owned(),
        ));
        queue.set_message(localized(
            "The batch stopped because no project is open",
            "没有打开的项目，批次已停止",
            "プロジェクトが開かれていないため停止しました",
        ));
        sync_batch(window, &queue);
        return;
    };
    let started = batch.borrow_mut().begin(index);
    if !started {
        return;
    }
    let request = batch.borrow().items()[index].request.clone();
    let runtime = export_runtime_settings(&settings.borrow());
    match manager
        .borrow_mut()
        .start(project_instance_id, project, request, runtime)
    {
        Ok(total_frames) => {
            planner.borrow_mut().reset();
            if let Some(workspace) = model.borrow_mut().current_workspace_mut() {
                workspace.pause_playback();
            }
            let (number, count) = {
                let queue = batch.borrow();
                (index + 1, queue.items().len())
            };
            window.set_exporting(true);
            window.set_export_progress(0.0);
            window.set_cancel_confirmation_visible(false);
            window.set_result_visible(false);
            window.set_progress_label(batch_progress_label(number, count, 0, total_frames).into());
            batch.borrow_mut().set_message(format!(
                "{} {number}/{count}",
                localized("Exporting item", "正在导出项", "書き出し中")
            ));
        }
        Err(error) => {
            let mut queue = batch.borrow_mut();
            queue.finish_running(BatchOutcome::Failed(error.to_string()));
            queue.set_message(error.to_string());
        }
    }
    sync_batch(window, &batch.borrow());
}

fn batch_progress_label(number: usize, count: usize, done: i32, total: i32) -> String {
    format!(
        "{} {number}/{count} · {done} / {total} {}",
        localized("Item", "项", "項目"),
        localized("frames", "帧", "フレーム")
    )
}

fn sync_export_window(
    window: &ExportWindow,
    workspace: &WorkspaceModel,
    settings: &SettingsStore,
    codecs: &mut ExportCodecState,
) {
    let project = workspace.project_settings();
    let duration = workspace.timeline_duration();
    window.set_project_width(project.width);
    window.set_project_height(project.height);
    window.set_project_fps(project.fps as f32);
    window.set_timeline_duration(duration);
    window.set_end_frame(duration);
    window.set_export_progress(0.0);
    window.set_progress_label(format!("0 / 0 {}", localized("frames", "帧", "フレーム")).into());
    window.set_cancel_confirmation_visible(false);

    let available_video = available_video_encoders();
    let (video_labels, video_values) = filtered_codec_catalog(&available_video, &VIDEO_CODECS);
    let default_video = setting_string(settings, "exportDefaultCodec", "libx264");
    let video_index = selected_codec_index(&video_values, &default_video);
    codecs.video_values = video_values;
    window.set_video_codec_labels(ModelRc::new(VecModel::from(video_labels)));
    window.set_video_codec_index(video_index);
    let video_codec = selected_codec(&codecs.video_values, video_index, "libx264");
    window.set_software_codec(is_software_codec(video_codec));

    let available_audio = available_audio_encoders();
    let (audio_labels, audio_values) = filtered_codec_catalog(&available_audio, &AUDIO_CODECS);
    let default_audio = match setting_string(settings, "exportDefaultAudioCodec", "aac").as_str() {
        "opus" => "libopus".to_owned(),
        value => value.to_owned(),
    };
    let audio_index = selected_codec_index(&audio_values, &default_audio);
    codecs.audio_values = audio_values;
    window.set_audio_codec_labels(ModelRc::new(VecModel::from(audio_labels)));
    window.set_audio_codec_index(audio_index);
    let audio_codec = selected_codec(&codecs.audio_values, audio_index, "aac");
    window.set_audio_bitrate_enabled(audio_codec != "flac" && audio_codec != "pcm_s16le");
}

fn current_export_project(model: &ApplicationModel) -> Option<(u64, ExportProject)> {
    let project_instance_id = model.current_project_instance_id()?;
    let workspace = model.current_workspace()?;
    let settings = workspace.project_settings();
    let duration = workspace.timeline_duration();
    Some((
        project_instance_id,
        ExportProject {
            width: settings.width,
            height: settings.height,
            fps: settings.fps,
            duration,
            sample_rate: settings.sample_rate,
        },
    ))
}

pub(super) fn export_workspace_for_frame(
    model: &mut ApplicationModel,
    project_instance_id: u64,
) -> Result<&mut WorkspaceModel, String> {
    if model.current_project_instance_id() != Some(project_instance_id) {
        return Err("Frame render error: project tab changed during export".to_owned());
    }
    model
        .current_workspace_mut()
        .ok_or_else(|| "Frame render error: export project is no longer available".to_owned())
}

fn export_request(
    window: &ExportWindow,
    codecs: &ExportCodecState,
) -> Result<ExportRequest, String> {
    let output_path = window.get_output_path().to_string();
    if !valid_export_path(&output_path) {
        return Err(localized(
            "Choose a valid export destination.",
            "请选择有效的导出位置。",
            "有効な書き出し先を指定してください",
        )
        .to_owned());
    }
    let video_codec = selected_codec(
        &codecs.video_values,
        window.get_video_codec_index(),
        "libx264",
    )
    .to_owned();
    let audio_codec =
        selected_codec(&codecs.audio_values, window.get_audio_codec_index(), "aac").to_owned();
    let preset = PRESET_VALUES
        .get(window.get_preset_index().max(0) as usize)
        .copied()
        .unwrap_or("medium")
        .to_owned();
    let profile = PROFILE_VALUES
        .get(window.get_profile_index().max(0) as usize)
        .copied()
        .unwrap_or("")
        .to_owned();
    let audio_bitrate_kbps = AUDIO_BITRATES
        .get(window.get_audio_bitrate_index().max(0) as usize)
        .copied()
        .unwrap_or(192);
    Ok(ExportRequest {
        mode: if window.get_format_index() == 1 {
            ExportMode::ImageSequence
        } else {
            ExportMode::Video
        },
        output_path: PathBuf::from(output_path.trim()),
        image_format: if window.get_image_format_index() == 1 {
            ExportImageFormat::Jpeg
        } else {
            ExportImageFormat::Png
        },
        video_codec,
        crf: window
            .get_crf_mode()
            .then_some(window.get_crf().round() as i32),
        bitrate_mbps: window.get_bitrate_mbps(),
        preset,
        profile,
        audio_codec,
        audio_bitrate_kbps,
        full_range: window.get_full_range(),
        start_frame: window.get_start_frame(),
        end_frame: window.get_end_frame(),
    })
}

fn export_runtime_settings(settings: &SettingsStore) -> ExportRuntimeSettings {
    ExportRuntimeSettings {
        image_quality: settings.i32_value("exportImageQuality", 95).clamp(1, 100) as u8,
        sequence_padding: settings.i32_value("exportSequencePadding", 6),
        progress_interval: settings.i32_value("exportProgressInterval", 5).max(1),
        max_plugin_block_size: usize::try_from(
            settings
                .i32_value("audioPluginMaxBlockSize", 1_024)
                .clamp(1, 8_192),
        )
        .unwrap_or(1_024),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn update_export(
    export: &slint::Weak<ExportWindow>,
    main: &slint::Weak<MainWindow>,
    timeline: &slint::Weak<TimelineWindow>,
    model: &Rc<RefCell<ApplicationModel>>,
    manager: &Rc<RefCell<ExportManager>>,
    planner: &Rc<RefCell<ExportPlannerRuntime>>,
    effect_catalog: &Rc<RefCell<EffectCatalog>>,
    settings: &Rc<RefCell<SettingsStore>>,
    batch: &Rc<RefCell<BatchQueue>>,
) {
    let Some(window) = export.upgrade() else {
        return;
    };
    // Collect first: the batch runner below borrows the manager again to start
    // the next item, which would panic while the poll borrow is still alive.
    let events = manager.borrow_mut().poll();
    for event in events {
        match event {
            ExportEvent::Progress(progress) => {
                window.set_export_progress(progress.progress as f32);
                let label = match batch.borrow().running_index() {
                    Some(index) => batch_progress_label(
                        index + 1,
                        batch.borrow().items().len(),
                        progress.current_frame,
                        progress.total_frames,
                    ),
                    None => export_progress_label(progress),
                };
                window.set_progress_label(label.into());
            }
            ExportEvent::Finished(result) => {
                planner.borrow_mut().reset();
                window.set_exporting(false);
                window.set_cancel_confirmation_visible(false);
                let batch_running = batch.borrow().running_index();
                if batch_running.is_none() {
                    show_export_result(&window, result.success, &result.message);
                    continue;
                }
                let outcome = if result.success {
                    BatchOutcome::Succeeded
                } else {
                    BatchOutcome::Failed(result.message.clone())
                };
                let Some(index) = batch.borrow_mut().finish_running(outcome) else {
                    continue;
                };
                let (next, total, pause) = {
                    let queue = batch.borrow();
                    (index + 1, queue.items().len(), queue.pause_requested())
                };
                if next >= total {
                    let (succeeded, failed) = batch.borrow().summary();
                    batch.borrow_mut().set_message(format!(
                        "{}: {succeeded} {} · {failed} {}",
                        localized("Batch finished", "批次完成", "バッチ完了"),
                        localized("succeeded", "成功", "成功"),
                        localized("failed", "失败", "失敗")
                    ));
                    sync_batch(&window, &batch.borrow());
                    if window.get_batch_completion_index() == 1 {
                        let _ = slint::quit_event_loop();
                    }
                } else if pause {
                    batch.borrow_mut().pause_after(next);
                    batch.borrow_mut().set_message(format!(
                        "{} {} · {}/{}",
                        localized("Paused", "已暂停", "一時停止しました"),
                        index + 1,
                        next + 1,
                        total
                    ));
                    sync_batch(&window, &batch.borrow());
                } else {
                    begin_batch_item(&window, model, manager, settings, planner, batch, next);
                }
            }
        }
    }

    let request = manager.borrow_mut().next_frame_request();
    let Some(request) = request else {
        return;
    };
    let planned = {
        let mut application = model.borrow_mut();
        match export_workspace_for_frame(&mut application, request.project_instance_id) {
            Ok(workspace) => {
                workspace.pause_playback();
                workspace.seek(request.frame);
                let project_path = workspace.project().path.clone();
                planner
                    .borrow_mut()
                    .build(
                        request.project_instance_id,
                        workspace,
                        request.frame,
                        &effect_catalog.borrow(),
                    )
                    .map(|planned| (planned, project_path))
                    .ok_or_else(|| {
                        format!("Frame render error: failed to plan frame {}", request.frame)
                    })
            }
            Err(message) => Err(message),
        }
    };
    sync_transport_weak(main, timeline, model);
    match planned {
        Ok((planned, project_path)) => {
            for warning in planned.warnings {
                eprintln!("Export planning warning: {warning}");
            }
            manager.borrow_mut().submit_scene(
                planned.scene,
                planned.audio,
                project_path.as_deref(),
            );
        }
        Err(message) => manager.borrow_mut().fail_current_frame(message),
    }
}

fn filtered_codec_catalog(
    available: &[String],
    catalog: &[(&str, &str)],
) -> (Vec<SharedString>, Vec<String>) {
    catalog
        .iter()
        .filter(|(_, value)| available.is_empty() || available.iter().any(|item| item == value))
        .map(|(label, value)| (SharedString::from(*label), (*value).to_owned()))
        .unzip()
}

pub(super) fn selected_codec_index(values: &[String], selected: &str) -> i32 {
    if values.is_empty() {
        return -1;
    }
    values
        .iter()
        .position(|value| value == selected)
        .unwrap_or(0) as i32
}

pub(super) fn selected_codec<'a>(values: &'a [String], index: i32, fallback: &'a str) -> &'a str {
    usize::try_from(index)
        .ok()
        .and_then(|index| values.get(index))
        .map(String::as_str)
        .unwrap_or(fallback)
}

fn export_progress_label(progress: freya_nle_export::ExportProgressPlan) -> String {
    let language = current_ui_language();
    let eta = match (language, progress.eta_seconds) {
        (_, seconds) if seconds <= 0 => String::new(),
        (UiLanguage::English, seconds) if seconds >= 3_600 => format!(
            " ({}h {}m remaining)",
            seconds / 3_600,
            seconds % 3_600 / 60
        ),
        (UiLanguage::SimplifiedChinese, seconds) if seconds >= 3_600 => format!(
            "（剩余 {} 小时 {} 分钟）",
            seconds / 3_600,
            seconds % 3_600 / 60
        ),
        (UiLanguage::Japanese, seconds) if seconds >= 3_600 => {
            format!(" (残り {}時間{}分)", seconds / 3_600, seconds % 3_600 / 60)
        }
        (UiLanguage::English, seconds) if seconds >= 60 => {
            format!(" ({}m {}s remaining)", seconds / 60, seconds % 60)
        }
        (UiLanguage::SimplifiedChinese, seconds) if seconds >= 60 => {
            format!("（剩余 {} 分 {} 秒）", seconds / 60, seconds % 60)
        }
        (UiLanguage::Japanese, seconds) if seconds >= 60 => {
            format!(" (残り {}分{}秒)", seconds / 60, seconds % 60)
        }
        (UiLanguage::English, seconds) => format!(" ({}s remaining)", seconds),
        (UiLanguage::SimplifiedChinese, seconds) => format!("（剩余 {} 秒）", seconds),
        (UiLanguage::Japanese, seconds) => format!(" (残り {}秒)", seconds),
    };
    format!(
        "{} / {} {}{}",
        progress.current_frame,
        progress.total_frames,
        localized("frames", "帧", "フレーム"),
        eta
    )
}

fn show_export_result(window: &ExportWindow, success: bool, message: &str) {
    window.set_result_success(success);
    window.set_result_message(message.into());
    window.set_result_visible(true);
}

fn choose_export_output(choose_folder: bool, current_path: &str) -> Option<PathBuf> {
    let current_path = PathBuf::from(current_path.trim());
    if choose_folder {
        let mut dialog = rfd::FileDialog::new().set_title(localized(
            "Choose the destination folder",
            "选择目标文件夹",
            "保存先フォルダを指定",
        ));
        if current_path.is_dir() {
            dialog = dialog.set_directory(current_path);
        }
        dialog.pick_folder()
    } else {
        let mut dialog = rfd::FileDialog::new()
            .set_title(localized(
                "Choose the export destination",
                "选择导出位置",
                "保存先を指定",
            ))
            .add_filter("MP4 Video", &["mp4"])
            .add_filter("MKV Video", &["mkv"])
            .add_filter("All Files", &["*"]);
        if let Some(parent) = current_path.parent().filter(|path| path.is_dir()) {
            dialog = dialog.set_directory(parent);
        }
        if let Some(name) = current_path.file_name().filter(|name| !name.is_empty()) {
            dialog = dialog.set_file_name(name.to_string_lossy().into_owned());
        }
        dialog.save_file()
    }
}
