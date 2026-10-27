#pragma once

#include <cstddef>
#include <cstdint>

inline constexpr std::uint32_t AVIQTL_RUST_CORE_ABI_VERSION = 2;

enum AviQtlCoreCapability : std::uint64_t {
    AVIQTL_RUST_CORE_CAPABILITY_NUMERIC_KEYFRAME_BATCH = 1ULL << 2,
    AVIQTL_RUST_CORE_CAPABILITY_AUDIO_BATCH_MIX = 1ULL << 5,
    AVIQTL_RUST_CORE_CAPABILITY_SETTINGS_DOCUMENT = 1ULL << 10,
    AVIQTL_RUST_CORE_CAPABILITY_PACKAGE_DOCUMENT = 1ULL << 12,
    AVIQTL_RUST_CORE_CAPABILITY_PERMISSION_STATE = 1ULL << 18,
    AVIQTL_RUST_CORE_CAPABILITY_SETTINGS_STATE = 1ULL << 19,
    AVIQTL_RUST_CORE_CAPABILITY_PACKAGE_CATALOG_STATE = 1ULL << 20,
    AVIQTL_RUST_CORE_CAPABILITY_EXPORT_PLANNING = 1ULL << 22,
};

enum AviQtlCoreStatus : std::uint32_t {
    AVIQTL_RUST_CORE_STATUS_OK = 0,
    AVIQTL_RUST_CORE_STATUS_INVALID_ARGUMENT = 1,
    AVIQTL_RUST_CORE_STATUS_OVERLAPPING_BUFFERS = 2,
    AVIQTL_RUST_CORE_STATUS_BUFFER_TOO_SMALL = 3,
    AVIQTL_RUST_CORE_STATUS_INVALID_JSON = 4,
    AVIQTL_RUST_CORE_STATUS_LOCKED_LAYER = 6,
};

struct AviQtlTimelineBakePlan;
struct AviQtlPermissionState;
struct AviQtlSettingsState;
struct AviQtlPackageCatalogState;
struct AviQtlEffectCatalogState;

struct AviQtlEasingParameters {
    double amplitude;
    double period;
};
static_assert(sizeof(AviQtlEasingParameters) == 16);
static_assert(alignof(AviQtlEasingParameters) == 8);
static_assert(offsetof(AviQtlEasingParameters, amplitude) == 0);
static_assert(offsetof(AviQtlEasingParameters, period) == 8);

struct AviQtlAudioMixParameters {
    double relative_time;
    double duration;
    float fade_in_seconds;
    float fade_out_seconds;
    float volume;
    float master_volume;
    float pan;
    std::uint32_t limiter;
};
static_assert(sizeof(AviQtlAudioMixParameters) == 40);
static_assert(alignof(AviQtlAudioMixParameters) == 8);
static_assert(offsetof(AviQtlAudioMixParameters, relative_time) == 0);
static_assert(offsetof(AviQtlAudioMixParameters, duration) == 8);
static_assert(offsetof(AviQtlAudioMixParameters, fade_in_seconds) == 16);
static_assert(offsetof(AviQtlAudioMixParameters, limiter) == 36);

struct AviQtlAudioMeter {
    float peak_left;
    float peak_right;
    float rms_left;
    float rms_right;
};
static_assert(sizeof(AviQtlAudioMeter) == 16);
static_assert(alignof(AviQtlAudioMeter) == 4);
static_assert(offsetof(AviQtlAudioMeter, peak_left) == 0);
static_assert(offsetof(AviQtlAudioMeter, rms_right) == 12);

#if INTPTR_MAX == INT64_MAX
static_assert(sizeof(AviQtlAudioBatchTrack) == 72);
static_assert(alignof(AviQtlAudioBatchTrack) == 8);
static_assert(offsetof(AviQtlAudioBatchTrack, parameters) == 16);
static_assert(offsetof(AviQtlAudioBatchTrack, clip_id) == 56);
static_assert(offsetof(AviQtlAudioBatchTrack, reserved) == 68);
#endif

struct AviQtlNumericKeyframe {
    std::int32_t frame;
    std::uint32_t interpolation;
    std::uint32_t step_frames;
    std::uint32_t custom_points_offset;
    std::uint32_t custom_points_length;
    std::uint32_t reserved;
    double value;
    double amplitude;
    double period;
};
static_assert(sizeof(AviQtlNumericKeyframe) == 48);
static_assert(alignof(AviQtlNumericKeyframe) == 8);
static_assert(offsetof(AviQtlNumericKeyframe, frame) == 0);
static_assert(offsetof(AviQtlNumericKeyframe, interpolation) == 4);
static_assert(offsetof(AviQtlNumericKeyframe, custom_points_length) == 16);
static_assert(offsetof(AviQtlNumericKeyframe, value) == 24);
static_assert(offsetof(AviQtlNumericKeyframe, period) == 40);

struct AviQtlNumericTrackView {
    const AviQtlNumericKeyframe *keyframes;
    std::size_t keyframes_length;
    const double *custom_points;
    std::size_t custom_points_length;
    double fallback_value;
};
static_assert(offsetof(AviQtlNumericTrackView, keyframes) == 0);
static_assert(offsetof(AviQtlNumericTrackView, fallback_value) == sizeof(void *) * 2 + sizeof(std::size_t) * 2);
#if INTPTR_MAX == INT64_MAX
static_assert(sizeof(AviQtlNumericTrackView) == 40);
static_assert(alignof(AviQtlNumericTrackView) == 8);
#endif

struct AviQtlRenderBakeInput {
    std::int32_t clip_id;
    std::int32_t layer;
    std::int32_t current_frame;
    std::int32_t start_frame;
    std::int32_t duration_frames;
    std::uint32_t clip_by_upper_object;
    std::uint16_t effect_count;
    std::uint16_t reserved;
    std::uint32_t effect_start_index;
    std::uint32_t has_transform;
    float x;
    float y;
    float z;
    float rotation_x;
    float rotation_y;
    float rotation_z;
    float scale;
    float opacity;
};
static_assert(sizeof(AviQtlRenderBakeInput) == 68);
static_assert(alignof(AviQtlRenderBakeInput) == 4);
static_assert(offsetof(AviQtlRenderBakeInput, clip_id) == 0);
static_assert(offsetof(AviQtlRenderBakeInput, effect_count) == 24);
static_assert(offsetof(AviQtlRenderBakeInput, effect_start_index) == 28);
static_assert(offsetof(AviQtlRenderBakeInput, x) == 36);
static_assert(offsetof(AviQtlRenderBakeInput, opacity) == 64);

struct AviQtlRenderBakeOutput {
    std::int32_t clip_id;
    std::int32_t layer;
    double time_position;
    std::int32_t start_frame;
    std::int32_t duration_frames;
    float x;
    float y;
    float z;
    float rotation_x;
    float rotation_y;
    float rotation_z;
    float scale_x;
    float scale_y;
    float opacity;
    std::uint32_t clip_by_upper_object;
    std::uint16_t effect_count;
    std::uint16_t reserved;
    std::uint32_t effect_start_index;
};
static_assert(sizeof(AviQtlRenderBakeOutput) == 72);
static_assert(alignof(AviQtlRenderBakeOutput) == 8);
static_assert(offsetof(AviQtlRenderBakeOutput, time_position) == 8);
static_assert(offsetof(AviQtlRenderBakeOutput, x) == 24);
static_assert(offsetof(AviQtlRenderBakeOutput, clip_by_upper_object) == 60);
static_assert(offsetof(AviQtlRenderBakeOutput, effect_start_index) == 68);

struct AviQtlAudioBakeInput {
    std::int32_t clip_id;
    std::int32_t start_frame;
    std::int32_t duration_frames;
    std::uint32_t has_audio_effect;
    double fps;
    float source_start_time;
    float speed_percent;
    float direct_time;
    float volume;
    float master_volume;
    float pan;
    float fade_in_seconds;
    float fade_out_seconds;
    std::uint32_t direct_mode;
    std::uint32_t mute;
    std::uint32_t solo;
    std::uint32_t limiter;
};
static_assert(sizeof(AviQtlAudioBakeInput) == 72);
static_assert(alignof(AviQtlAudioBakeInput) == 8);
static_assert(offsetof(AviQtlAudioBakeInput, fps) == 16);
static_assert(offsetof(AviQtlAudioBakeInput, source_start_time) == 24);
static_assert(offsetof(AviQtlAudioBakeInput, direct_mode) == 56);
static_assert(offsetof(AviQtlAudioBakeInput, limiter) == 68);

struct AviQtlAudioBakeOutput {
    std::int32_t clip_id;
    std::int32_t start_frame;
    std::int32_t duration_frames;
    float source_start_time;
    float playback_speed;
    float direct_time;
    float volume;
    float master_volume;
    float pan;
    float fade_in_seconds;
    float fade_out_seconds;
    std::uint32_t mute;
    std::uint32_t solo;
    std::uint32_t limiter;
    std::uint32_t direct_mode;
};
static_assert(sizeof(AviQtlAudioBakeOutput) == 60);
static_assert(alignof(AviQtlAudioBakeOutput) == 4);
static_assert(offsetof(AviQtlAudioBakeOutput, source_start_time) == 12);
static_assert(offsetof(AviQtlAudioBakeOutput, mute) == 44);
static_assert(offsetof(AviQtlAudioBakeOutput, direct_mode) == 56);

struct AviQtlEffectParamEntry {
    std::uint32_t clip_id;
    std::uint8_t effect_index;
    std::uint8_t param_type;
    std::uint8_t reserved[2];
    std::uint8_t param_name[20];
    float value[4];
};
static_assert(sizeof(AviQtlEffectParamEntry) == 44);
static_assert(alignof(AviQtlEffectParamEntry) == 4);
static_assert(offsetof(AviQtlEffectParamEntry, param_name) == 8);
static_assert(offsetof(AviQtlEffectParamEntry, value) == 28);

struct AviQtlSceneBakeCounts {
    std::size_t render_count;
    std::size_t audio_count;
    std::size_t param_count;
    std::uint64_t clips_visited;
    std::uint64_t selected_effect_count;
    std::uint64_t numeric_batch_calls;
    std::uint64_t numeric_track_count;
};
static_assert(sizeof(AviQtlSceneBakeCounts) == 56);
static_assert(alignof(AviQtlSceneBakeCounts) == 8);
static_assert(offsetof(AviQtlSceneBakeCounts, clips_visited) == 24);

struct AviQtlTimelineClipGeometry {
    std::int32_t clip_id;
    std::int32_t layer;
    std::int32_t start_frame;
    std::int32_t duration_frames;
};
static_assert(sizeof(AviQtlTimelineClipGeometry) == 16);
static_assert(alignof(AviQtlTimelineClipGeometry) == 4);
static_assert(offsetof(AviQtlTimelineClipGeometry, clip_id) == 0);
static_assert(offsetof(AviQtlTimelineClipGeometry, duration_frames) == 12);

struct AviQtlTimelineMoveInput {
    std::int32_t clip_id;
    std::int32_t old_layer;
    std::int32_t old_start_frame;
    std::int32_t duration_frames;
    std::int32_t target_layer;
    std::int32_t target_start_frame;
};
static_assert(sizeof(AviQtlTimelineMoveInput) == 24);
static_assert(alignof(AviQtlTimelineMoveInput) == 4);
static_assert(offsetof(AviQtlTimelineMoveInput, clip_id) == 0);
static_assert(offsetof(AviQtlTimelineMoveInput, target_layer) == 16);

struct AviQtlSceneSettings {
    std::int32_t width;
    std::int32_t height;
    double fps;
    std::int32_t total_frames;
    std::uint32_t grid_mode;
    double grid_bpm;
    double grid_offset;
    std::int32_t grid_interval;
    std::int32_t grid_subdivision;
    std::uint32_t enable_snap;
    std::int32_t magnetic_snap_range;
};
static_assert(sizeof(AviQtlSceneSettings) == 56);
static_assert(alignof(AviQtlSceneSettings) == 8);
static_assert(offsetof(AviQtlSceneSettings, width) == 0);
static_assert(offsetof(AviQtlSceneSettings, fps) == 8);
static_assert(offsetof(AviQtlSceneSettings, grid_bpm) == 24);
static_assert(offsetof(AviQtlSceneSettings, magnetic_snap_range) == 52);

extern "C" {

std::uint32_t aviqtl_core_abi_version();
std::uint64_t aviqtl_core_capabilities();

std::uint32_t aviqtl_numeric_keyframe_batch_evaluate(const AviQtlNumericTrackView *tracks, std::size_t tracksLength, std::int32_t frame, double *output, std::size_t outputLength);
std::uint32_t aviqtl_numeric_keyframe_evaluate_typed(const AviQtlNumericTrackView *track, std::int32_t frame, std::uint32_t discrete, double *output);
std::uint32_t aviqtl_numeric_interpolation_from_name(const std::uint8_t *value, std::size_t valueLength);

std::uint32_t aviqtl_timeline_bake_plan_create(const std::uint8_t *input, std::size_t inputLength, AviQtlTimelineBakePlan **outputHandle);
void aviqtl_timeline_bake_plan_destroy(AviQtlTimelineBakePlan *handle);
std::uint32_t aviqtl_timeline_bake_plan_reset(AviQtlTimelineBakePlan *handle, const std::uint8_t *input, std::size_t inputLength);
std::size_t aviqtl_timeline_bake_plan_effect_count(AviQtlTimelineBakePlan *handle);
std::uint32_t aviqtl_timeline_bake_plan_evaluate(AviQtlTimelineBakePlan *handle, std::int32_t currentFrame, std::uint32_t fullBake, std::int32_t prefetchFrames, AviQtlRenderBakeOutput *renderOutput, std::size_t renderCapacity, AviQtlAudioBakeOutput *audioOutput, std::size_t audioCapacity, AviQtlEffectParamEntry *paramOutput, std::size_t paramCapacity, AviQtlSceneBakeCounts *counts);

                                      std::int32_t *outputPrimary);

std::uint32_t aviqtl_permission_state_create(const std::uint8_t *input, std::size_t inputLength, AviQtlPermissionState **outputHandle);
void aviqtl_permission_state_destroy(AviQtlPermissionState *handle);
std::uint32_t aviqtl_permission_state_reset(AviQtlPermissionState *handle, const std::uint8_t *input, std::size_t inputLength);
std::uint32_t aviqtl_permission_state_snapshot_json(const AviQtlPermissionState *handle, std::uint8_t *output, std::size_t outputCapacity, std::size_t *outputLength);
std::uint32_t aviqtl_permission_state_has(const AviQtlPermissionState *handle, const std::uint8_t *plugin, std::size_t pluginLength, std::int32_t permission);
std::uint32_t aviqtl_permission_state_grant(AviQtlPermissionState *handle, const std::uint8_t *plugin, std::size_t pluginLength, std::int32_t permission);
std::uint32_t aviqtl_permission_state_revoke(AviQtlPermissionState *handle, const std::uint8_t *plugin, std::size_t pluginLength, std::int32_t permission, std::uint32_t *pluginExisted);
std::uint32_t aviqtl_permission_state_grant_all(AviQtlPermissionState *handle, const std::uint8_t *plugin, std::size_t pluginLength);
std::uint32_t aviqtl_permission_state_revoke_all(AviQtlPermissionState *handle, const std::uint8_t *plugin, std::size_t pluginLength);
std::uint64_t aviqtl_permission_state_mask(const AviQtlPermissionState *handle, const std::uint8_t *plugin, std::size_t pluginLength);
std::uint32_t aviqtl_permission_state_is_authorized(const AviQtlPermissionState *handle, const std::uint8_t *plugin, std::size_t pluginLength);

std::uint32_t aviqtl_settings_state_create_defaults(const std::uint8_t *platformDefaults, std::size_t platformDefaultsLength, AviQtlSettingsState **outputHandle);
std::uint32_t aviqtl_settings_state_create(const std::uint8_t *input, std::size_t inputLength, AviQtlSettingsState **outputHandle);
void aviqtl_settings_state_destroy(AviQtlSettingsState *handle);
std::uint32_t aviqtl_settings_state_reset(AviQtlSettingsState *handle, const std::uint8_t *input, std::size_t inputLength);
std::uint32_t aviqtl_settings_state_merge_json(AviQtlSettingsState *handle, const std::uint8_t *loaded, std::size_t loadedLength, std::uint32_t *migrated);
std::uint32_t aviqtl_settings_state_snapshot_json(const AviQtlSettingsState *handle, std::uint8_t *output, std::size_t outputCapacity, std::size_t *outputLength);
std::uint32_t aviqtl_settings_state_persistent_json(const AviQtlSettingsState *handle, std::uint8_t *output, std::size_t outputCapacity, std::size_t *outputLength);
std::uint32_t aviqtl_settings_state_set_value_json(AviQtlSettingsState *handle, const std::uint8_t *key, std::size_t keyLength, const std::uint8_t *valueDocument, std::size_t valueDocumentLength, std::uint32_t *changed, std::uint32_t *persistent);
std::uint32_t aviqtl_settings_state_remove_value(AviQtlSettingsState *handle, const std::uint8_t *key, std::size_t keyLength, std::uint32_t *changed, std::uint32_t *persistent);
std::int32_t aviqtl_settings_state_get_i32(const AviQtlSettingsState *handle, const std::uint8_t *key, std::size_t keyLength, std::int32_t fallback);
double aviqtl_settings_state_get_f64(const AviQtlSettingsState *handle, const std::uint8_t *key, std::size_t keyLength, double fallback);
std::uint32_t aviqtl_settings_state_get_bool(const AviQtlSettingsState *handle, const std::uint8_t *key, std::size_t keyLength, std::uint32_t fallback);

                                       std::size_t keyframesLength, std::uint8_t *output, std::size_t outputCapacity, std::size_t *outputLength);
                                           std::size_t *outputLength);

std::uint32_t aviqtl_package_document_apply_json(const std::uint8_t *input, std::size_t inputLength, std::uint8_t *output, std::size_t outputCapacity, std::size_t *outputLength);
std::uint32_t aviqtl_package_catalog_state_create(AviQtlPackageCatalogState **outputHandle);
void aviqtl_package_catalog_state_destroy(AviQtlPackageCatalogState *handle);
std::uint32_t aviqtl_package_catalog_state_clear(AviQtlPackageCatalogState *handle);
std::uint32_t aviqtl_package_catalog_state_merge_json(AviQtlPackageCatalogState *handle, const std::uint8_t *input, std::size_t inputLength);
std::uint32_t aviqtl_package_catalog_state_snapshot_json(const AviQtlPackageCatalogState *handle, std::uint8_t *output, std::size_t outputCapacity, std::size_t *outputLength);
std::uint32_t aviqtl_package_catalog_state_has_updates(const AviQtlPackageCatalogState *handle);
std::uint32_t aviqtl_package_catalog_state_find_json(const AviQtlPackageCatalogState *handle, const std::uint8_t *packageId, std::size_t packageIdLength, const std::uint8_t *sourceRepository, std::size_t sourceRepositoryLength, std::uint8_t *output, std::size_t outputCapacity, std::size_t *outputLength);
std::uint32_t aviqtl_package_catalog_state_filter_json(const AviQtlPackageCatalogState *handle, const std::uint8_t *filter, std::size_t filterLength, std::uint8_t *output, std::size_t outputCapacity, std::size_t *outputLength);
std::uint32_t aviqtl_package_catalog_state_upgrade_ids_json(const AviQtlPackageCatalogState *handle, std::uint8_t *output, std::size_t outputCapacity, std::size_t *outputLength);
std::uint32_t aviqtl_package_catalog_state_set_installed(AviQtlPackageCatalogState *handle, const std::uint8_t *packageId, std::size_t packageIdLength, const std::uint8_t *version, std::size_t versionLength, std::uint32_t installed, std::uint32_t *changed);

}
