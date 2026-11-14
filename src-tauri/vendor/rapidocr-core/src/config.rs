//! Configuration types for the OCR pipeline and individual model stages.
//!
//! The configuration can be serialized as TOML and is shared by the library API
//! and the CLI. Stage-specific configs are required only when the corresponding
//! [`PipelineConfig`] flag is enabled.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Full OCR pipeline configuration.
pub struct RapidOcrConfig {
    /// Stage switches controlling detection, direction classification, and recognition.
    #[serde(default)]
    pub pipeline: PipelineConfig,
    /// ONNX Runtime session resource limits shared by all enabled stages.
    #[serde(default)]
    pub inference: InferenceOptions,
    /// Minimum accepted recognition confidence for detected text lines.
    pub text_score: f32,
    /// Minimum image side length used by the high-level pipeline resize step.
    pub min_side_len: u32,
    /// Maximum image side length used by the high-level pipeline resize step.
    pub max_side_len: u32,
    /// Minimum detector input height after vertical padding.
    pub min_height: u32,
    /// Maximum width-to-height ratio before vertical padding is applied, or `-1.0` to disable the ratio gate.
    pub width_height_ratio: f32,
    /// Detection-stage configuration, required when [`PipelineConfig::use_det`] is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub det: Option<DetConfig>,
    /// Text-line direction classifier configuration, required when [`PipelineConfig::use_cls`] is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cls: Option<ClsConfig>,
    /// Recognition-stage configuration, required when [`PipelineConfig::use_rec`] is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rec: Option<RecConfig>,
}

impl RapidOcrConfig {
    /// Builds the default `ppocrv6-small` configuration rooted at `model_dir`.
    pub fn ppocr_v6_small(model_dir: impl Into<PathBuf>) -> Self {
        crate::model::PPOCRV6_SMALL.config(model_dir)
    }

    /// Reads and validates a TOML configuration file.
    pub fn from_toml_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read config {}", path.display()))?;
        Self::from_toml_str(&content)
            .with_context(|| format!("failed to load config {}", path.display()))
    }

    /// Parses and validates a TOML configuration string.
    ///
    /// Legacy top-level `use_det`, `use_cls`, and `use_rec` fields are accepted
    /// and mapped into the nested [`PipelineConfig`].
    pub fn from_toml_str(content: &str) -> Result<Self> {
        let mut parsed: RapidOcrConfigFile =
            toml::from_str(content).context("failed to parse config TOML")?;

        if let Some(use_det) = parsed.use_det {
            parsed.cfg.pipeline.use_det = use_det;
        }
        if let Some(use_cls) = parsed.use_cls {
            parsed.cfg.pipeline.use_cls = use_cls;
        }
        if let Some(use_rec) = parsed.use_rec {
            parsed.cfg.pipeline.use_rec = use_rec;
        }

        parsed.cfg.validate()?;
        Ok(parsed.cfg)
    }

    /// Writes this configuration as pretty TOML after validation.
    pub fn write_toml_file(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        self.validate().context("invalid OCR config")?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create config dir {}", parent.display()))?;
        }
        let content = self.to_toml_string()?;
        fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
    }

    /// Serializes this configuration as pretty TOML after validation.
    pub fn to_toml_string(&self) -> Result<String> {
        self.validate().context("invalid OCR config")?;
        toml::to_string_pretty(self).context("failed to serialize config")
    }

    /// Validates field ranges and enabled-stage requirements.
    pub fn validate(&self) -> Result<()> {
        self.pipeline.validate()?;
        self.inference.validate()?;
        ensure_unit_interval(self.text_score, "text_score")?;
        ensure!(self.min_side_len > 0, "min_side_len must be greater than 0");
        ensure!(self.max_side_len > 0, "max_side_len must be greater than 0");
        ensure!(
            self.min_side_len <= self.max_side_len,
            "min_side_len must be less than or equal to max_side_len"
        );
        ensure!(self.min_height > 0, "min_height must be greater than 0");
        ensure!(
            self.width_height_ratio == -1.0 || self.width_height_ratio > 0.0,
            "width_height_ratio must be positive or -1"
        );

        if self.pipeline.use_det {
            self.det
                .as_ref()
                .context("pipeline.use_det is true but [det] config is missing")?
                .validate()?;
        }
        if self.pipeline.use_cls {
            self.cls
                .as_ref()
                .context("pipeline.use_cls is true but [cls] config is missing")?
                .validate()?;
        }
        if self.pipeline.use_rec {
            self.rec
                .as_ref()
                .context("pipeline.use_rec is true but [rec] config is missing")?
                .validate()?;
        }

        Ok(())
    }

    /// Returns a copy of this configuration with replacement pipeline flags.
    pub fn with_pipeline(mut self, pipeline: PipelineConfig) -> Self {
        self.pipeline = pipeline;
        self
    }

    /// Returns a copy of this configuration with replacement ONNX Runtime options.
    pub fn with_inference_options(mut self, inference: InferenceOptions) -> Self {
        self.inference = inference;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
/// ONNX Runtime session resource configuration shared by all enabled stages.
pub struct InferenceOptions {
    /// Number of threads used to parallelize execution within an operator.
    pub intra_threads: usize,
    /// Number of threads used to parallelize graph execution when enabled.
    pub inter_threads: usize,
    /// Enables parallel graph execution, which can increase memory usage.
    pub parallel_execution: bool,
    /// Enables ONNX Runtime's CPU arena allocator.
    pub enable_cpu_mem_arena: bool,
    /// Execution provider used by each ONNX Runtime session.
    #[serde(default)]
    pub execution_provider: ExecutionProvider,
}

impl InferenceOptions {
    /// Validates ONNX Runtime thread limits.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=i32::MAX as usize).contains(&self.intra_threads),
            "inference.intra_threads must be between 1 and {}",
            i32::MAX
        );
        ensure!(
            (1..=i32::MAX as usize).contains(&self.inter_threads),
            "inference.inter_threads must be between 1 and {}",
            i32::MAX
        );
        if self.execution_provider == ExecutionProvider::DirectMl {
            ensure!(
                cfg!(feature = "directml"),
                "inference.execution_provider = \"direct-ml\" requires the `directml` Cargo feature"
            );
            ensure!(
                cfg!(target_os = "windows"),
                "the DirectML execution provider is only available on Windows"
            );
            ensure!(
                !self.parallel_execution,
                "inference.parallel_execution must be false when using DirectML"
            );
        }
        Ok(())
    }
}

impl Default for InferenceOptions {
    fn default() -> Self {
        Self {
            intra_threads: 1,
            inter_threads: 1,
            parallel_execution: false,
            enable_cpu_mem_arena: false,
            execution_provider: ExecutionProvider::Cpu,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
/// ONNX Runtime execution provider selection.
pub enum ExecutionProvider {
    /// Use the default CPU execution provider.
    #[default]
    Cpu,
    /// Use DirectML on Windows. Requires the `directml` Cargo feature.
    DirectMl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Pipeline stage switches.
pub struct PipelineConfig {
    /// Enables text box detection.
    pub use_det: bool,
    /// Enables text-line orientation classification before recognition.
    pub use_cls: bool,
    /// Enables text recognition.
    pub use_rec: bool,
}

impl PipelineConfig {
    /// Enables detection, classification, and recognition.
    pub const fn full() -> Self {
        Self {
            use_det: true,
            use_cls: true,
            use_rec: true,
        }
    }

    /// Enables detection and recognition, skipping orientation classification.
    pub const fn without_cls() -> Self {
        Self {
            use_det: true,
            use_cls: false,
            use_rec: true,
        }
    }

    /// Enables detection only.
    pub const fn detection_only() -> Self {
        Self {
            use_det: true,
            use_cls: false,
            use_rec: false,
        }
    }

    /// Enables recognition on the whole input image, without detection or classification.
    pub const fn recognition_only() -> Self {
        Self {
            use_det: false,
            use_cls: false,
            use_rec: true,
        }
    }

    /// Validates stage combinations.
    pub fn validate(&self) -> Result<()> {
        if !self.use_det && !self.use_rec {
            bail!("at least one of pipeline.use_det or pipeline.use_rec must be true");
        }
        if self.use_cls && !self.use_rec {
            bail!("pipeline.use_cls requires pipeline.use_rec because cls only rotates recognition crops");
        }
        Ok(())
    }
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self::full()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Detection-stage model and postprocessing configuration.
pub struct DetConfig {
    /// ONNX detection model path.
    pub model_path: PathBuf,
    /// Side-length limit applied before detection inference.
    pub limit_side_len: u32,
    /// Whether [`DetConfig::limit_side_len`] applies to the minimum or maximum image side.
    pub limit_type: LimitType,
    /// Resource policy applied to the detector tensor after the requested resize is calculated.
    #[serde(default)]
    pub input_limits: DetInputLimits,
    /// Per-channel input normalization mean in RGB order.
    pub mean: [f32; 3],
    /// Per-channel input normalization standard deviation in RGB order.
    pub std: [f32; 3],
    /// Probability threshold used to build the DB text mask.
    pub thresh: f32,
    /// Minimum candidate box confidence accepted by DB postprocessing.
    pub box_thresh: f32,
    /// Maximum number of detection candidates returned.
    pub max_candidates: usize,
    /// Expansion ratio applied to DB candidate polygons.
    pub unclip_ratio: f32,
    /// Minimum candidate size in detector-map pixels.
    pub min_size: u32,
}

impl DetConfig {
    /// Validates model paths, numeric ranges, and normalization parameters.
    pub fn validate(&self) -> Result<()> {
        ensure_non_empty_path(&self.model_path, "det.model_path")?;
        ensure!(
            self.limit_side_len > 0,
            "det.limit_side_len must be greater than 0"
        );
        self.input_limits.validate()?;
        ensure_finite_array(self.mean, "det.mean")?;
        ensure_finite_array(self.std, "det.std")?;
        for (idx, value) in self.std.iter().enumerate() {
            ensure!(*value != 0.0, "det.std[{idx}] must not be 0");
        }
        ensure_unit_interval(self.thresh, "det.thresh")?;
        ensure_unit_interval(self.box_thresh, "det.box_thresh")?;
        ensure!(
            self.max_candidates > 0,
            "det.max_candidates must be greater than 0"
        );
        ensure!(
            self.unclip_ratio.is_finite() && self.unclip_ratio > 0.0,
            "det.unclip_ratio must be finite and greater than 0"
        );
        ensure!(self.min_size > 0, "det.min_size must be greater than 0");
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Side selected by detector resize limiting.
pub enum LimitType {
    /// Limit the minimum side.
    Min,
    /// Limit the maximum side.
    Max,
}

/// Resource limits for the dynamically shaped detection input tensor.
///
/// The safe default bounds both the longest side and total pixel count. Applications
/// can reject oversized detector inputs or explicitly opt into unrestricted resizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DetInputLimits {
    /// Maximum detector input side after 32-pixel alignment, or no side limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_side_len: Option<u32>,
    /// Maximum detector input pixel count after 32-pixel alignment, or no pixel limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_pixels: Option<u64>,
    /// Behavior when the resize requested by [`DetConfig::limit_side_len`] exceeds a limit.
    pub overflow_behavior: DetOverflowBehavior,
}

impl DetInputLimits {
    /// Validates limits against the detector's required 32-pixel alignment.
    pub fn validate(&self) -> Result<()> {
        if let Some(max_side_len) = self.max_side_len {
            ensure!(
                max_side_len >= 32,
                "det.input_limits.max_side_len must be at least 32"
            );
        }
        if let Some(max_pixels) = self.max_pixels {
            ensure!(
                max_pixels >= 32 * 32,
                "det.input_limits.max_pixels must be at least 1024"
            );
        }
        Ok(())
    }

    /// Returns an explicit unrestricted policy for callers willing to accept resource risk.
    pub const fn unrestricted() -> Self {
        Self {
            max_side_len: None,
            max_pixels: None,
            overflow_behavior: DetOverflowBehavior::Allow,
        }
    }
}

impl Default for DetInputLimits {
    fn default() -> Self {
        Self {
            max_side_len: Some(4096),
            max_pixels: Some(2048 * 2048),
            overflow_behavior: DetOverflowBehavior::Downscale,
        }
    }
}

/// Behavior when detector preprocessing would exceed configured resource limits.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DetOverflowBehavior {
    /// Preserve the image aspect ratio while shrinking it into the configured limits.
    #[default]
    Downscale,
    /// Return an error before allocating the oversized detector tensor.
    Reject,
    /// Ignore detector input limits and perform the requested resize.
    Allow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Recognition-stage model configuration.
pub struct RecConfig {
    /// ONNX recognition model path.
    pub model_path: PathBuf,
    /// Recognition dictionary path, one character or token per line.
    pub dict_path: PathBuf,
    /// Recognition input tensor shape as `[channels, height, width]`.
    pub image_shape: [usize; 3],
    /// Maximum number of crops processed per inference call.
    pub batch_size: usize,
}

impl RecConfig {
    /// Validates model paths, input shape, and batch size.
    pub fn validate(&self) -> Result<()> {
        ensure_non_empty_path(&self.model_path, "rec.model_path")?;
        ensure_non_empty_path(&self.dict_path, "rec.dict_path")?;
        ensure_image_shape(self.image_shape, "rec.image_shape")?;
        ensure!(self.batch_size > 0, "rec.batch_size must be greater than 0");
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Text-line orientation classifier configuration.
pub struct ClsConfig {
    /// ONNX classification model path.
    pub model_path: PathBuf,
    /// Classification input tensor shape as `[channels, height, width]`.
    pub image_shape: [usize; 3],
    /// Maximum number of crops processed per inference call.
    pub batch_size: usize,
    /// Minimum confidence required before rotating a crop.
    pub thresh: f32,
    /// Class labels in model output order, commonly `["0", "180"]`.
    pub labels: Vec<String>,
}

impl ClsConfig {
    /// Validates model path, input shape, batch size, threshold, and labels.
    pub fn validate(&self) -> Result<()> {
        ensure_non_empty_path(&self.model_path, "cls.model_path")?;
        ensure_image_shape(self.image_shape, "cls.image_shape")?;
        ensure!(self.batch_size > 0, "cls.batch_size must be greater than 0");
        ensure_unit_interval(self.thresh, "cls.thresh")?;
        ensure!(!self.labels.is_empty(), "cls.labels must not be empty");
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct RapidOcrConfigFile {
    #[serde(flatten)]
    cfg: RapidOcrConfig,
    #[serde(default)]
    use_det: Option<bool>,
    #[serde(default)]
    use_cls: Option<bool>,
    #[serde(default)]
    use_rec: Option<bool>,
}

fn ensure_non_empty_path(path: &Path, label: &str) -> Result<()> {
    ensure!(!path.as_os_str().is_empty(), "{label} must not be empty");
    Ok(())
}

fn ensure_unit_interval(value: f32, label: &str) -> Result<()> {
    ensure!(
        value.is_finite() && (0.0..=1.0).contains(&value),
        "{label} must be finite and between 0 and 1"
    );
    Ok(())
}

fn ensure_finite_array(values: [f32; 3], label: &str) -> Result<()> {
    for (idx, value) in values.iter().enumerate() {
        ensure!(value.is_finite(), "{label}[{idx}] must be finite");
    }
    Ok(())
}

fn ensure_image_shape(shape: [usize; 3], label: &str) -> Result<()> {
    ensure!(shape[0] == 3, "{label}[0] must be 3 channels");
    ensure!(shape[1] > 0, "{label}[1] must be greater than 0");
    ensure!(shape[2] > 0, "{label}[2] must be greater than 0");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_toml_roundtrip_preserves_default_paths_and_flags() {
        let path = std::env::temp_dir().join(format!(
            "rapidocr-rs-config-roundtrip-{}.toml",
            std::process::id()
        ));
        let cfg = RapidOcrConfig::ppocr_v6_small("models");
        cfg.write_toml_file(&path).unwrap();

        let loaded = RapidOcrConfig::from_toml_file(&path).unwrap();
        let _ = fs::remove_file(&path);

        assert_eq!(loaded.pipeline, PipelineConfig::full());
        assert_eq!(loaded.inference, InferenceOptions::default());
        assert_eq!(loaded.min_height, 30);
        assert_eq!(loaded.width_height_ratio, 8.0);
        let det = loaded.det.as_ref().unwrap();
        assert_eq!(det.input_limits, DetInputLimits::default());
        let cls = loaded.cls.as_ref().unwrap();
        let rec = loaded.rec.as_ref().unwrap();
        assert_eq!(
            det.model_path,
            PathBuf::from("models/PP-OCRv6_det_small.onnx")
        );
        assert_eq!(
            cls.model_path,
            PathBuf::from("models/ch_ppocr_mobile_v2.0_cls_mobile.onnx")
        );
        assert_eq!(rec.dict_path, PathBuf::from("models/ppocrv6_dict.txt"));
        assert_eq!(cls.labels, vec!["0", "180"]);
    }

    #[test]
    fn config_without_detector_input_limits_uses_safe_defaults() {
        let content = RapidOcrConfig::ppocr_v6_small("models")
            .to_toml_string()
            .unwrap();
        let limits_section = "\n[det.input_limits]\nmax_side_len = 4096\nmax_pixels = 4194304\noverflow_behavior = \"downscale\"\n";
        let content = content.replace(limits_section, "");

        let loaded = RapidOcrConfig::from_toml_str(&content).unwrap();

        assert_eq!(loaded.det.unwrap().input_limits, DetInputLimits::default());
    }

    #[test]
    fn detector_input_limits_reject_values_below_alignment_minimum() {
        let mut cfg = RapidOcrConfig::ppocr_v6_small("models");
        cfg.det.as_mut().unwrap().input_limits.max_side_len = Some(31);

        let error = cfg.validate().unwrap_err().to_string();

        assert!(error.contains("det.input_limits.max_side_len must be at least 32"));
    }

    #[test]
    fn legacy_top_level_pipeline_flags_are_loaded() {
        let mut content = RapidOcrConfig::ppocr_v6_small("models")
            .to_toml_string()
            .unwrap();
        content = content.replace(
            "[pipeline]\nuse_det = true\nuse_cls = true\nuse_rec = true\n\n",
            "",
        );
        content.insert_str(0, "use_det = false\nuse_cls = false\nuse_rec = true\n");

        let loaded = RapidOcrConfig::from_toml_str(&content).unwrap();

        assert_eq!(loaded.pipeline, PipelineConfig::recognition_only());
    }

    #[test]
    fn config_validation_rejects_cls_without_rec() {
        let cfg = RapidOcrConfig::ppocr_v6_small("models").with_pipeline(PipelineConfig {
            use_det: true,
            use_cls: true,
            use_rec: false,
        });

        let err = cfg.validate().unwrap_err().to_string();

        assert!(err.contains("pipeline.use_cls requires pipeline.use_rec"));
    }

    #[test]
    fn detection_only_config_does_not_require_recognition_assets() {
        let mut cfg = RapidOcrConfig::ppocr_v6_small("models")
            .with_pipeline(PipelineConfig::detection_only());
        cfg.cls = None;
        cfg.rec = None;

        cfg.validate().unwrap();
    }

    #[test]
    fn full_pipeline_requires_recognition_config() {
        let mut cfg = RapidOcrConfig::ppocr_v6_small("models");
        cfg.rec = None;

        let err = cfg.validate().unwrap_err().to_string();

        assert!(err.contains("pipeline.use_rec is true but [rec] config is missing"));
    }

    #[test]
    fn config_without_inference_section_uses_resource_limited_defaults() {
        let content = RapidOcrConfig::ppocr_v6_small("models")
            .to_toml_string()
            .unwrap();
        let content = content.replace(
            "[inference]\nintra_threads = 1\ninter_threads = 1\nparallel_execution = false\nenable_cpu_mem_arena = false\nexecution_provider = \"cpu\"\n\n",
            "",
        );

        let loaded = RapidOcrConfig::from_toml_str(&content).unwrap();

        assert_eq!(loaded.inference, InferenceOptions::default());
    }

    #[test]
    fn config_validation_rejects_zero_inference_threads() {
        let mut cfg = RapidOcrConfig::ppocr_v6_small("models");
        cfg.inference.intra_threads = 0;

        let err = cfg.validate().unwrap_err().to_string();

        assert!(err.contains("inference.intra_threads must be between 1 and"));
    }

    #[test]
    fn config_validation_rejects_inference_threads_above_ffi_range() {
        let mut cfg = RapidOcrConfig::ppocr_v6_small("models");
        cfg.inference.inter_threads = i32::MAX as usize + 1;

        let err = cfg.validate().unwrap_err().to_string();

        assert!(err.contains("inference.inter_threads must be between 1 and"));
    }

    #[test]
    fn partial_inference_section_uses_defaults_for_omitted_fields() {
        let content = RapidOcrConfig::ppocr_v6_small("models")
            .to_toml_string()
            .unwrap();
        let content = content.replace(
            "[inference]\nintra_threads = 1\ninter_threads = 1\nparallel_execution = false\nenable_cpu_mem_arena = false\nexecution_provider = \"cpu\"",
            "[inference]\nexecution_provider = \"cpu\"",
        );

        let loaded = RapidOcrConfig::from_toml_str(&content).unwrap();

        assert_eq!(loaded.inference, InferenceOptions::default());
    }

    #[test]
    fn directml_config_rejects_parallel_execution() {
        let mut cfg = RapidOcrConfig::ppocr_v6_small("models");
        cfg.inference.execution_provider = ExecutionProvider::DirectMl;
        cfg.inference.parallel_execution = true;

        let err = cfg.validate().unwrap_err().to_string();

        #[cfg(feature = "directml")]
        assert!(err.contains("must be false when using DirectML"));
        #[cfg(not(feature = "directml"))]
        assert!(err.contains("requires the `directml` Cargo feature"));
    }

    #[cfg(feature = "directml")]
    #[test]
    fn directml_config_is_valid_with_serial_execution_on_windows() {
        let mut cfg = RapidOcrConfig::ppocr_v6_small("models");
        cfg.inference.execution_provider = ExecutionProvider::DirectMl;

        if cfg!(target_os = "windows") {
            cfg.validate().unwrap();
        } else {
            assert!(cfg
                .validate()
                .unwrap_err()
                .to_string()
                .contains("only available on Windows"));
        }
    }
}
