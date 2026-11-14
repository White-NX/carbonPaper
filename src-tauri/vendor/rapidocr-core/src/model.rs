//! Registered model sets and local model-cache helpers.
//!
//! Model assets are not bundled with the crate. [`ModelCache`] can download
//! registered assets on demand or verify that an application-provided cache is
//! complete for the selected [`PipelineConfig`].

use std::{
    fs,
    path::{Path, PathBuf},
};

#[cfg(feature = "model-download")]
use std::io::Write;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::config::{
    ClsConfig, DetConfig, DetInputLimits, LimitType, PipelineConfig, RapidOcrConfig, RecConfig,
};

const PPOCRV6_DET_SMALL_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv6/det/PP-OCRv6_det_small.onnx";
const PPOCRV6_DET_SMALL_SHA256: &str =
    "090f04abcd9d9a7498bc4ebf677e4cb9bdce1fe4197ddb7e529f1ef44e1ff94f";
const PPOCRV6_REC_SMALL_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv6/rec/PP-OCRv6_rec_small.onnx";
const PPOCRV6_REC_SMALL_SHA256: &str =
    "6f327246b50388f3c176ae304bd95767ea6dc0c9ae92153ef8cbe210b3c14884";
const PPOCRV6_DET_TINY_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv6/det/PP-OCRv6_det_tiny.onnx";
const PPOCRV6_DET_TINY_SHA256: &str =
    "f42c0fbd294d95eac1a550e131b277dac97462c8025fa4b6c3cec1b7894bd3d5";
const PPOCRV6_REC_TINY_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv6/rec/PP-OCRv6_rec_tiny.onnx";
const PPOCRV6_REC_TINY_SHA256: &str =
    "e16e242de5937ad92609223f19bc2aff3727ee40b095f996907c24749bad251b";
const PPOCRV6_DET_MEDIUM_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv6/det/PP-OCRv6_det_medium.onnx";
const PPOCRV6_DET_MEDIUM_SHA256: &str =
    "92078b7355007ccfffcd4c8cd441a3afd4538904d06881b29a155e1e679907c2";
const PPOCRV6_REC_MEDIUM_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv6/rec/PP-OCRv6_rec_medium.onnx";
const PPOCRV6_REC_MEDIUM_SHA256: &str =
    "eef444829dbbe18d7fea59a3f6eb75647518d2b3a9568d27c92e42940204894b";
const PPOCRV4_CLS_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv4/cls/ch_ppocr_mobile_v2.0_cls_mobile.onnx";
const PPOCRV4_CLS_SHA256: &str = "e47acedf663230f8863ff1ab0e64dd2d82b838fceb5957146dab185a89d6215c";
const PPOCRV6_DICT_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master/paddle/PP-OCRv6/rec/PP-OCRv6_rec_small/ppocrv6_dict.txt";
const PPOCRV6_DICT_SHA256: &str =
    "b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d";
const PPOCRV6_TINY_DICT_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master/paddle/PP-OCRv6/rec/PP-OCRv6_rec_tiny/ppocrv6_tiny_dict.txt";
const PPOCRV6_TINY_DICT_SHA256: &str =
    "c5cbe34ef40c29c4df07ed012bf96569cb69a2d2a01a07027e9f13cb832bd9cd";
const PPOCRV4_EN_DET_MOBILE_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv4/det/en_PP-OCRv3_det_mobile.onnx";
const PPOCRV4_EN_DET_MOBILE_SHA256: &str =
    "ea07c15d38ac40cd69da3c493444ec75b44ff23840553ff8ba102c1219ed39c2";
const PPOCRV4_EN_REC_MOBILE_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv4/rec/en_PP-OCRv4_rec_mobile.onnx";
const PPOCRV4_EN_REC_MOBILE_SHA256: &str =
    "e8770c967605983d1570cdf5352041dfb68fa0c21664f49f47b155abd3e0e318";
const PPOCRV4_EN_DICT_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/paddle/PP-OCRv4/rec/en_PP-OCRv4_rec_mobile/en_dict.txt";
const PPOCRV4_EN_DICT_SHA256: &str =
    "5662df9d2d03f0e8ca0d3b0649d6acbab904b6a14b3d3521463c71c37c668ce3";
const PPOCRV5_CH_DET_MOBILE_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv5/det/ch_PP-OCRv5_det_mobile.onnx";
const PPOCRV5_CH_DET_MOBILE_SHA256: &str =
    "4d97c44a20d30a81aad087d6a396b08f786c4635742afc391f6621f5c6ae78ae";
const PPOCRV5_CH_REC_MOBILE_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv5/rec/ch_PP-OCRv5_rec_mobile.onnx";
const PPOCRV5_CH_REC_MOBILE_SHA256: &str =
    "5825fc7ebf84ae7a412be049820b4d86d77620f204a041697b0494669b1742c5";
const PPOCRV5_CH_DET_SERVER_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv5/det/ch_PP-OCRv5_det_server.onnx";
const PPOCRV5_CH_DET_SERVER_SHA256: &str =
    "0f8846b1d4bba223a2a2f9d9b44022fbc22cc019051a602b41a7fda9667e4cad";
const PPOCRV5_CH_REC_SERVER_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv5/rec/ch_PP-OCRv5_rec_server.onnx";
const PPOCRV5_CH_REC_SERVER_SHA256: &str =
    "e09385400eaaaef34ceff54aeb7c4f0f1fe014c27fa8b9905d4709b65746562a";
const PPOCRV5_EN_REC_MOBILE_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv5/rec/en_PP-OCRv5_rec_mobile.onnx";
const PPOCRV5_EN_REC_MOBILE_SHA256: &str =
    "c3461add59bb4323ecba96a492ab75e06dda42467c9e3d0c18db5d1d21924be8";
const PPOCRV5_CLS_MOBILE_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv5/cls/ch_PP-LCNet_x0_25_textline_ori_cls_mobile.onnx";
const PPOCRV5_CLS_MOBILE_SHA256: &str =
    "54379ae5174d026780215fc748a7f31910dee36818e63d49e17dc598ecc82df7";
const PPOCRV5_CLS_SERVER_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv5/cls/ch_PP-LCNet_x1_0_textline_ori_cls_server.onnx";
const PPOCRV5_CLS_SERVER_SHA256: &str =
    "7d3c02ef6c7da8ae08b4347cc7695b2081aae68c325d64375724ecf39c99e743";
const PPOCRV5_DICT_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/paddle/PP-OCRv5/rec/ch_PP-OCRv5_rec_server/ppocrv5_dict.txt";
const PPOCRV5_DICT_SHA256: &str =
    "d1979e9f794c464c0d2e0b70a7fe14dd978e9dc644c0e71f14158cdf8342af1b";
const PPOCRV5_EN_DICT_URL: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.0/paddle/PP-OCRv5/rec/en_PP-OCRv5_rec_mobile/ppocrv5_en_dict.txt";
const PPOCRV5_EN_DICT_SHA256: &str =
    "e025a66d31f327ba0c232e03f407ae8d105e1e709e7ccb3f408aa778c24e70d6";
/// Default registered model-set name used by the CLI and examples.
pub const DEFAULT_MODEL_SET_NAME: &str = "ppocrv6-small";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Kind of model asset in a registered model set.
pub enum ModelAssetKind {
    /// Text detection ONNX model.
    Detection,
    /// Text-line orientation classifier ONNX model.
    Classification,
    /// Text recognition ONNX model.
    Recognition,
    /// Recognition dictionary text file.
    Dictionary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Downloadable or cacheable model asset.
pub struct ModelAssetSpec {
    /// Human-readable asset name used in errors.
    pub name: &'static str,
    /// Stage or file kind.
    pub kind: ModelAssetKind,
    /// Filename expected inside a [`ModelCache`] root.
    pub filename: &'static str,
    /// Download URL.
    pub url: &'static str,
    /// Optional SHA-256 checksum used after download and when reusing cached files.
    pub sha256: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// Complete detection-classification-recognition model set.
pub struct ModelSetSpec {
    /// Stable model-set identifier.
    pub name: &'static str,
    /// Model family label, such as `PP-OCRv6`.
    pub family: &'static str,
    /// Detection model and parameters.
    pub det: DetModelSpec,
    /// Classification model and parameters.
    pub cls: ClsModelSpec,
    /// Recognition model, dictionary, and parameters.
    pub rec: RecModelSpec,
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// Detection model parameters for a registered model set.
pub struct DetModelSpec {
    /// Detection ONNX asset.
    pub asset: ModelAssetSpec,
    /// Detector resize side limit.
    pub limit_side_len: u32,
    /// Detector resize limit mode.
    pub limit_type: LimitType,
    /// Input normalization mean in RGB order.
    pub mean: [f32; 3],
    /// Input normalization standard deviation in RGB order.
    pub std: [f32; 3],
    /// DB mask threshold.
    pub thresh: f32,
    /// DB candidate score threshold.
    pub box_thresh: f32,
    /// Maximum number of detection candidates.
    pub max_candidates: usize,
    /// Polygon expansion ratio for DB boxes.
    pub unclip_ratio: f32,
    /// Minimum candidate size in detector-map pixels.
    pub min_size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// Text-line orientation classifier parameters for a registered model set.
pub struct ClsModelSpec {
    /// Classification ONNX asset.
    pub asset: ModelAssetSpec,
    /// Input tensor shape as `[channels, height, width]`.
    pub image_shape: [usize; 3],
    /// Maximum crops per inference call.
    pub batch_size: usize,
    /// Minimum score required before a `180` label rotates the crop.
    pub thresh: f32,
    /// Output labels in model order.
    pub labels: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// Recognition model parameters for a registered model set.
pub struct RecModelSpec {
    /// Recognition ONNX asset.
    pub asset: ModelAssetSpec,
    /// Recognition dictionary asset.
    pub dict: ModelAssetSpec,
    /// Input tensor shape as `[channels, height, width]`.
    pub image_shape: [usize; 3],
    /// Maximum crops per inference call.
    pub batch_size: usize,
}

impl ModelSetSpec {
    /// Returns all assets in the model set.
    pub fn assets(&self) -> [ModelAssetSpec; 4] {
        [
            self.det.asset,
            self.cls.asset,
            self.rec.asset,
            self.rec.dict,
        ]
    }

    /// Returns only the assets required by the selected pipeline stages.
    pub fn assets_for_pipeline(&self, pipeline: PipelineConfig) -> Vec<ModelAssetSpec> {
        let mut assets = Vec::new();
        if pipeline.use_det {
            assets.push(self.det.asset);
        }
        if pipeline.use_cls {
            assets.push(self.cls.asset);
        }
        if pipeline.use_rec {
            assets.push(self.rec.asset);
            assets.push(self.rec.dict);
        }
        assets
    }

    /// Builds a [`RapidOcrConfig`] with model paths rooted at `model_dir`.
    pub fn config(&self, model_dir: impl Into<PathBuf>) -> RapidOcrConfig {
        let model_dir = model_dir.into();
        RapidOcrConfig {
            pipeline: PipelineConfig::full(),
            inference: Default::default(),
            text_score: 0.5,
            min_side_len: 30,
            max_side_len: 2000,
            min_height: 30,
            width_height_ratio: 8.0,
            det: Some(DetConfig {
                model_path: model_dir.join(self.det.asset.filename),
                limit_side_len: self.det.limit_side_len,
                limit_type: self.det.limit_type,
                input_limits: DetInputLimits::default(),
                mean: self.det.mean,
                std: self.det.std,
                thresh: self.det.thresh,
                box_thresh: self.det.box_thresh,
                max_candidates: self.det.max_candidates,
                unclip_ratio: self.det.unclip_ratio,
                min_size: self.det.min_size,
            }),
            cls: Some(ClsConfig {
                model_path: model_dir.join(self.cls.asset.filename),
                image_shape: self.cls.image_shape,
                batch_size: self.cls.batch_size,
                thresh: self.cls.thresh,
                labels: self
                    .cls
                    .labels
                    .iter()
                    .map(|label| label.to_string())
                    .collect(),
            }),
            rec: Some(RecConfig {
                model_path: model_dir.join(self.rec.asset.filename),
                dict_path: model_dir.join(self.rec.dict.filename),
                image_shape: self.rec.image_shape,
                batch_size: self.rec.batch_size,
            }),
        }
    }
}

const PPOCRV6_DET_TINY: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv6 tiny detection model",
    kind: ModelAssetKind::Detection,
    filename: "PP-OCRv6_det_tiny.onnx",
    url: PPOCRV6_DET_TINY_URL,
    sha256: Some(PPOCRV6_DET_TINY_SHA256),
};

const PPOCRV6_DET_SMALL: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv6 small detection model",
    kind: ModelAssetKind::Detection,
    filename: "PP-OCRv6_det_small.onnx",
    url: PPOCRV6_DET_SMALL_URL,
    sha256: Some(PPOCRV6_DET_SMALL_SHA256),
};

const PPOCRV6_DET_MEDIUM: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv6 medium detection model",
    kind: ModelAssetKind::Detection,
    filename: "PP-OCRv6_det_medium.onnx",
    url: PPOCRV6_DET_MEDIUM_URL,
    sha256: Some(PPOCRV6_DET_MEDIUM_SHA256),
};

const PPOCRV6_REC_TINY: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv6 tiny recognition model",
    kind: ModelAssetKind::Recognition,
    filename: "PP-OCRv6_rec_tiny.onnx",
    url: PPOCRV6_REC_TINY_URL,
    sha256: Some(PPOCRV6_REC_TINY_SHA256),
};

const PPOCRV6_REC_SMALL: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv6 small recognition model",
    kind: ModelAssetKind::Recognition,
    filename: "PP-OCRv6_rec_small.onnx",
    url: PPOCRV6_REC_SMALL_URL,
    sha256: Some(PPOCRV6_REC_SMALL_SHA256),
};

const PPOCRV6_REC_MEDIUM: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv6 medium recognition model",
    kind: ModelAssetKind::Recognition,
    filename: "PP-OCRv6_rec_medium.onnx",
    url: PPOCRV6_REC_MEDIUM_URL,
    sha256: Some(PPOCRV6_REC_MEDIUM_SHA256),
};

const PPOCRV6_DICT: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv6 recognition dictionary",
    kind: ModelAssetKind::Dictionary,
    filename: "ppocrv6_dict.txt",
    url: PPOCRV6_DICT_URL,
    sha256: Some(PPOCRV6_DICT_SHA256),
};

const PPOCRV6_TINY_DICT: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv6 tiny recognition dictionary",
    kind: ModelAssetKind::Dictionary,
    filename: "ppocrv6_tiny_dict.txt",
    url: PPOCRV6_TINY_DICT_URL,
    sha256: Some(PPOCRV6_TINY_DICT_SHA256),
};

const PPOCRV4_CLS: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv4 text direction classifier",
    kind: ModelAssetKind::Classification,
    filename: "ch_ppocr_mobile_v2.0_cls_mobile.onnx",
    url: PPOCRV4_CLS_URL,
    sha256: Some(PPOCRV4_CLS_SHA256),
};

const PPOCRV4_EN_DET_MOBILE: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv4 English mobile detection model",
    kind: ModelAssetKind::Detection,
    filename: "en_PP-OCRv3_det_mobile.onnx",
    url: PPOCRV4_EN_DET_MOBILE_URL,
    sha256: Some(PPOCRV4_EN_DET_MOBILE_SHA256),
};

const PPOCRV4_EN_REC_MOBILE: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv4 English mobile recognition model",
    kind: ModelAssetKind::Recognition,
    filename: "en_PP-OCRv4_rec_mobile.onnx",
    url: PPOCRV4_EN_REC_MOBILE_URL,
    sha256: Some(PPOCRV4_EN_REC_MOBILE_SHA256),
};

const PPOCRV4_EN_DICT: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv4 English recognition dictionary",
    kind: ModelAssetKind::Dictionary,
    filename: "en_dict.txt",
    url: PPOCRV4_EN_DICT_URL,
    sha256: Some(PPOCRV4_EN_DICT_SHA256),
};

const PPOCRV5_CH_DET_MOBILE: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv5 Chinese mobile detection model",
    kind: ModelAssetKind::Detection,
    filename: "ch_PP-OCRv5_det_mobile.onnx",
    url: PPOCRV5_CH_DET_MOBILE_URL,
    sha256: Some(PPOCRV5_CH_DET_MOBILE_SHA256),
};

const PPOCRV5_CH_DET_SERVER: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv5 Chinese server detection model",
    kind: ModelAssetKind::Detection,
    filename: "ch_PP-OCRv5_det_server.onnx",
    url: PPOCRV5_CH_DET_SERVER_URL,
    sha256: Some(PPOCRV5_CH_DET_SERVER_SHA256),
};

const PPOCRV5_CH_REC_SERVER: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv5 Chinese server recognition model",
    kind: ModelAssetKind::Recognition,
    filename: "ch_PP-OCRv5_rec_server.onnx",
    url: PPOCRV5_CH_REC_SERVER_URL,
    sha256: Some(PPOCRV5_CH_REC_SERVER_SHA256),
};

const PPOCRV5_CH_REC_MOBILE: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv5 Chinese mobile recognition model",
    kind: ModelAssetKind::Recognition,
    filename: "ch_PP-OCRv5_rec_mobile.onnx",
    url: PPOCRV5_CH_REC_MOBILE_URL,
    sha256: Some(PPOCRV5_CH_REC_MOBILE_SHA256),
};

const PPOCRV5_EN_REC_MOBILE: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv5 English mobile recognition model",
    kind: ModelAssetKind::Recognition,
    filename: "en_PP-OCRv5_rec_mobile.onnx",
    url: PPOCRV5_EN_REC_MOBILE_URL,
    sha256: Some(PPOCRV5_EN_REC_MOBILE_SHA256),
};

const PPOCRV5_CLS_MOBILE: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv5 mobile text-line orientation classifier",
    kind: ModelAssetKind::Classification,
    filename: "ch_PP-LCNet_x0_25_textline_ori_cls_mobile.onnx",
    url: PPOCRV5_CLS_MOBILE_URL,
    sha256: Some(PPOCRV5_CLS_MOBILE_SHA256),
};

const PPOCRV5_CLS_SERVER: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv5 server text-line orientation classifier",
    kind: ModelAssetKind::Classification,
    filename: "ch_PP-LCNet_x1_0_textline_ori_cls_server.onnx",
    url: PPOCRV5_CLS_SERVER_URL,
    sha256: Some(PPOCRV5_CLS_SERVER_SHA256),
};

const PPOCRV5_DICT: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv5 recognition dictionary",
    kind: ModelAssetKind::Dictionary,
    filename: "ppocrv5_dict.txt",
    url: PPOCRV5_DICT_URL,
    sha256: Some(PPOCRV5_DICT_SHA256),
};

const PPOCRV5_EN_DICT: ModelAssetSpec = ModelAssetSpec {
    name: "PP-OCRv5 English recognition dictionary",
    kind: ModelAssetKind::Dictionary,
    filename: "ppocrv5_en_dict.txt",
    url: PPOCRV5_EN_DICT_URL,
    sha256: Some(PPOCRV5_EN_DICT_SHA256),
};

const PPOCRV6_DET_PARAMS: DetModelSpec = DetModelSpec {
    asset: PPOCRV6_DET_SMALL,
    limit_side_len: 736,
    limit_type: LimitType::Min,
    mean: [0.5, 0.5, 0.5],
    std: [0.5, 0.5, 0.5],
    thresh: 0.3,
    box_thresh: 0.5,
    max_candidates: 1000,
    unclip_ratio: 1.6,
    min_size: 3,
};

const PPOCRV4_CLS_PARAMS: ClsModelSpec = ClsModelSpec {
    asset: PPOCRV4_CLS,
    image_shape: [3, 48, 192],
    batch_size: 6,
    thresh: 0.9,
    labels: &["0", "180"],
};

const PPOCRV5_CLS_PARAMS: ClsModelSpec = ClsModelSpec {
    asset: PPOCRV5_CLS_MOBILE,
    image_shape: [3, 80, 160],
    batch_size: 6,
    thresh: 0.9,
    labels: &["0", "180"],
};

const PPOCRV5_SERVER_CLS_PARAMS: ClsModelSpec = ClsModelSpec {
    asset: PPOCRV5_CLS_SERVER,
    image_shape: [3, 80, 160],
    batch_size: 6,
    thresh: 0.9,
    labels: &["0", "180"],
};

const fn det_with_asset(asset: ModelAssetSpec) -> DetModelSpec {
    DetModelSpec {
        asset,
        ..PPOCRV6_DET_PARAMS
    }
}

const fn rec_with_assets(asset: ModelAssetSpec, dict: ModelAssetSpec) -> RecModelSpec {
    RecModelSpec {
        asset,
        dict,
        image_shape: [3, 48, 320],
        batch_size: 6,
    }
}

/// Registered PP-OCRv6 tiny model set.
pub const PPOCRV6_TINY: ModelSetSpec = ModelSetSpec {
    name: "ppocrv6-tiny",
    family: "PP-OCRv6",
    det: det_with_asset(PPOCRV6_DET_TINY),
    cls: PPOCRV4_CLS_PARAMS,
    rec: rec_with_assets(PPOCRV6_REC_TINY, PPOCRV6_TINY_DICT),
};

/// Registered PP-OCRv6 small model set.
pub const PPOCRV6_SMALL: ModelSetSpec = ModelSetSpec {
    name: "ppocrv6-small",
    family: "PP-OCRv6",
    det: det_with_asset(PPOCRV6_DET_SMALL),
    cls: PPOCRV4_CLS_PARAMS,
    rec: rec_with_assets(PPOCRV6_REC_SMALL, PPOCRV6_DICT),
};

/// Registered PP-OCRv6 medium model set.
pub const PPOCRV6_MEDIUM: ModelSetSpec = ModelSetSpec {
    name: "ppocrv6-medium",
    family: "PP-OCRv6",
    det: det_with_asset(PPOCRV6_DET_MEDIUM),
    cls: PPOCRV4_CLS_PARAMS,
    rec: rec_with_assets(PPOCRV6_REC_MEDIUM, PPOCRV6_DICT),
};

/// Registered PP-OCRv4 English mobile model set.
pub const PPOCRV4_EN_MOBILE: ModelSetSpec = ModelSetSpec {
    name: "ppocrv4-en-mobile",
    family: "PP-OCRv4",
    det: det_with_asset(PPOCRV4_EN_DET_MOBILE),
    cls: PPOCRV4_CLS_PARAMS,
    rec: rec_with_assets(PPOCRV4_EN_REC_MOBILE, PPOCRV4_EN_DICT),
};

/// Registered PP-OCRv5 English mobile model set.
pub const PPOCRV5_EN_MOBILE: ModelSetSpec = ModelSetSpec {
    name: "ppocrv5-en-mobile",
    family: "PP-OCRv5",
    det: det_with_asset(PPOCRV5_CH_DET_MOBILE),
    cls: PPOCRV5_CLS_PARAMS,
    rec: rec_with_assets(PPOCRV5_EN_REC_MOBILE, PPOCRV5_EN_DICT),
};

/// Registered PP-OCRv5 Chinese mobile model set.
pub const PPOCRV5_CH_MOBILE: ModelSetSpec = ModelSetSpec {
    name: "ppocrv5-ch-mobile",
    family: "PP-OCRv5",
    det: det_with_asset(PPOCRV5_CH_DET_MOBILE),
    cls: PPOCRV5_CLS_PARAMS,
    rec: rec_with_assets(PPOCRV5_CH_REC_MOBILE, PPOCRV5_DICT),
};

/// Registered PP-OCRv5 Chinese server model set.
pub const PPOCRV5_CH_SERVER: ModelSetSpec = ModelSetSpec {
    name: "ppocrv5-ch-server",
    family: "PP-OCRv5",
    det: det_with_asset(PPOCRV5_CH_DET_SERVER),
    cls: PPOCRV5_SERVER_CLS_PARAMS,
    rec: rec_with_assets(PPOCRV5_CH_REC_SERVER, PPOCRV5_DICT),
};

static MODEL_SETS: [ModelSetSpec; 7] = [
    PPOCRV6_SMALL,
    PPOCRV6_TINY,
    PPOCRV6_MEDIUM,
    PPOCRV4_EN_MOBILE,
    PPOCRV5_CH_MOBILE,
    PPOCRV5_EN_MOBILE,
    PPOCRV5_CH_SERVER,
];

/// Returns registered model sets in CLI display order.
pub fn available_model_sets() -> &'static [ModelSetSpec] {
    &MODEL_SETS
}

/// Returns registered model-set names in CLI display order.
pub fn available_model_set_names() -> Vec<&'static str> {
    available_model_sets()
        .iter()
        .map(|model_set| model_set.name)
        .collect()
}

/// Finds a registered model set by name, ignoring ASCII case.
pub fn model_set_by_name(name: &str) -> Option<&'static ModelSetSpec> {
    available_model_sets()
        .iter()
        .find(|model_set| model_set.name.eq_ignore_ascii_case(name))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Model-cache preparation mode.
pub enum ModelDownloadMode {
    /// Download any missing asset.
    Missing,
    /// Never download; return an error if any required asset is missing.
    Never,
}

#[derive(Debug, Clone)]
/// Local directory containing model assets.
pub struct ModelCache {
    root: PathBuf,
}

impl ModelCache {
    /// Creates a model cache rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Returns the model cache root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the expected local path for an asset.
    pub fn asset_path(&self, asset: ModelAssetSpec) -> PathBuf {
        self.root.join(asset.filename)
    }

    /// Builds a pipeline configuration for `model_set` using this cache root.
    pub fn config_for(&self, model_set: &ModelSetSpec) -> RapidOcrConfig {
        model_set.config(&self.root)
    }

    /// Lists all missing assets for a complete model set.
    pub fn missing_assets(&self, model_set: &ModelSetSpec) -> Vec<ModelAssetSpec> {
        model_set
            .assets()
            .into_iter()
            .filter(|asset| !self.asset_path(*asset).exists())
            .collect()
    }

    /// Lists missing assets required by the selected pipeline stages.
    pub fn missing_assets_for_pipeline(
        &self,
        model_set: &ModelSetSpec,
        pipeline: PipelineConfig,
    ) -> Vec<ModelAssetSpec> {
        model_set
            .assets_for_pipeline(pipeline)
            .into_iter()
            .filter(|asset| !self.asset_path(*asset).exists())
            .collect()
    }

    /// Ensures all assets for a complete model set exist and pass checksum validation.
    pub fn ensure_model_set(
        &self,
        model_set: &ModelSetSpec,
        mode: ModelDownloadMode,
    ) -> Result<()> {
        self.ensure_assets(model_set.assets(), mode)
    }

    /// Ensures assets needed by selected pipeline stages exist and pass checksum validation.
    pub fn ensure_model_set_for_pipeline(
        &self,
        model_set: &ModelSetSpec,
        pipeline: PipelineConfig,
        mode: ModelDownloadMode,
    ) -> Result<()> {
        self.ensure_assets(model_set.assets_for_pipeline(pipeline), mode)
    }

    /// Ensures the default `ppocrv6-small` model set exists in the cache.
    pub fn ensure_ppocrv6_small(&self, mode: ModelDownloadMode) -> Result<()> {
        self.ensure_model_set(&PPOCRV6_SMALL, mode)
    }

    fn ensure_assets(
        &self,
        assets: impl IntoIterator<Item = ModelAssetSpec>,
        mode: ModelDownloadMode,
    ) -> Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("failed to create model cache {}", self.root.display()))?;

        for asset in assets {
            self.ensure_asset(asset, mode)
                .with_context(|| format!("failed to prepare {}", asset.name))?;
        }
        Ok(())
    }

    fn ensure_asset(&self, asset: ModelAssetSpec, mode: ModelDownloadMode) -> Result<()> {
        let path = self.asset_path(asset);
        if path.exists() {
            if let Some(expected) = asset.sha256 {
                verify_sha256(&path, expected)?;
            }
            return Ok(());
        }

        if mode == ModelDownloadMode::Never {
            bail!(
                "missing model asset {} at {}; allow downloads or place the file in the model cache",
                asset.name,
                path.display()
            );
        }

        download_asset(asset, &path)
    }
}

/// Ensures default `ppocrv6-small` models exist and returns the cache path.
pub fn ensure_ppocrv6_small_models(model_dir: impl AsRef<Path>) -> Result<PathBuf> {
    let cache = ModelCache::new(model_dir.as_ref());
    cache.ensure_ppocrv6_small(ModelDownloadMode::Missing)?;
    Ok(cache.root().to_path_buf())
}

#[cfg(feature = "model-download")]
fn download_asset(asset: ModelAssetSpec, path: &Path) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 rapidocr-rs")
        .build()
        .context("failed to build HTTP client")?;
    let bytes = client
        .get(asset.url)
        .send()
        .with_context(|| format!("failed to download {} from {}", asset.name, asset.url))?
        .error_for_status()
        .with_context(|| format!("download returned an error for {}", asset.url))?
        .bytes()
        .with_context(|| format!("failed to read response body for {}", asset.url))?;

    let mut file = fs::File::create(path)
        .with_context(|| format!("failed to create model file {}", path.display()))?;
    file.write_all(&bytes)
        .with_context(|| format!("failed to write model file {}", path.display()))?;

    if let Some(expected) = asset.sha256 {
        verify_sha256(path, expected)?;
    }
    Ok(())
}

#[cfg(not(feature = "model-download"))]
fn download_asset(asset: ModelAssetSpec, path: &Path) -> Result<()> {
    bail!(
        "missing model asset {} at {}; rapidocr-core was built without the `model-download` feature, so enable it or place the file in the model cache",
        asset.name,
        path.display()
    )
}

fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let bytes =
        fs::read(path).with_context(|| format!("failed to read model file {}", path.display()))?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != expected {
        bail!(
            "sha256 mismatch for {}: expected {}, got {}",
            path.display(),
            expected,
            actual
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppocrv6_small_model_set_builds_default_config_paths() {
        let cfg = PPOCRV6_SMALL.config("models");

        assert_eq!(
            cfg.det.as_ref().unwrap().model_path,
            PathBuf::from("models/PP-OCRv6_det_small.onnx")
        );
        assert_eq!(
            cfg.cls.as_ref().unwrap().model_path,
            PathBuf::from("models/ch_ppocr_mobile_v2.0_cls_mobile.onnx")
        );
        assert_eq!(
            cfg.rec.as_ref().unwrap().dict_path,
            PathBuf::from("models/ppocrv6_dict.txt")
        );
    }

    #[test]
    fn non_default_model_sets_build_data_driven_configs() {
        let v4 = PPOCRV4_EN_MOBILE.config("models");
        assert_eq!(PPOCRV4_EN_MOBILE.family, "PP-OCRv4");
        assert_eq!(
            v4.det.as_ref().unwrap().model_path,
            PathBuf::from("models/en_PP-OCRv3_det_mobile.onnx")
        );
        assert_eq!(
            v4.rec.as_ref().unwrap().model_path,
            PathBuf::from("models/en_PP-OCRv4_rec_mobile.onnx")
        );
        assert_eq!(
            v4.rec.as_ref().unwrap().dict_path,
            PathBuf::from("models/en_dict.txt")
        );
        assert_eq!(v4.cls.as_ref().unwrap().image_shape, [3, 48, 192]);

        let v5 = PPOCRV5_EN_MOBILE.config("models");
        assert_eq!(PPOCRV5_EN_MOBILE.family, "PP-OCRv5");
        assert_eq!(
            v5.det.as_ref().unwrap().model_path,
            PathBuf::from("models/ch_PP-OCRv5_det_mobile.onnx")
        );
        assert_eq!(
            v5.rec.as_ref().unwrap().model_path,
            PathBuf::from("models/en_PP-OCRv5_rec_mobile.onnx")
        );
        assert_eq!(
            v5.rec.as_ref().unwrap().dict_path,
            PathBuf::from("models/ppocrv5_en_dict.txt")
        );
        assert_eq!(v5.cls.as_ref().unwrap().image_shape, [3, 80, 160]);

        let v5_server = PPOCRV5_CH_SERVER.config("models");
        assert_eq!(
            v5_server.det.as_ref().unwrap().model_path,
            PathBuf::from("models/ch_PP-OCRv5_det_server.onnx")
        );
        assert_eq!(
            v5_server.cls.as_ref().unwrap().model_path,
            PathBuf::from("models/ch_PP-LCNet_x1_0_textline_ori_cls_server.onnx")
        );
        assert_eq!(
            v5_server.rec.as_ref().unwrap().model_path,
            PathBuf::from("models/ch_PP-OCRv5_rec_server.onnx")
        );
        assert_eq!(
            v5_server.rec.as_ref().unwrap().dict_path,
            PathBuf::from("models/ppocrv5_dict.txt")
        );
    }

    #[test]
    fn model_set_selects_assets_for_pipeline_switches() {
        let without_cls = PPOCRV6_SMALL.assets_for_pipeline(PipelineConfig::without_cls());
        assert_eq!(without_cls.len(), 3);
        assert!(!without_cls
            .iter()
            .any(|asset| asset.kind == ModelAssetKind::Classification));

        let detection_only = PPOCRV6_SMALL.assets_for_pipeline(PipelineConfig::detection_only());
        assert_eq!(detection_only.len(), 1);
        assert_eq!(detection_only[0].kind, ModelAssetKind::Detection);

        let recognition_only =
            PPOCRV6_SMALL.assets_for_pipeline(PipelineConfig::recognition_only());
        assert_eq!(recognition_only.len(), 2);
        assert!(recognition_only
            .iter()
            .any(|asset| asset.kind == ModelAssetKind::Recognition));
        assert!(recognition_only
            .iter()
            .any(|asset| asset.kind == ModelAssetKind::Dictionary));
    }

    #[test]
    fn model_registry_exposes_default_model_set_by_name() {
        assert_eq!(DEFAULT_MODEL_SET_NAME, PPOCRV6_SMALL.name);
        assert_eq!(
            available_model_set_names(),
            vec![
                "ppocrv6-small",
                "ppocrv6-tiny",
                "ppocrv6-medium",
                "ppocrv4-en-mobile",
                "ppocrv5-ch-mobile",
                "ppocrv5-en-mobile",
                "ppocrv5-ch-server"
            ]
        );

        let model_set = model_set_by_name("ppocrv6-small").unwrap();
        assert_eq!(model_set.name, PPOCRV6_SMALL.name);

        let model_set = model_set_by_name("PPOCRV6-SMALL").unwrap();
        assert_eq!(model_set.name, PPOCRV6_SMALL.name);

        let model_set = model_set_by_name("ppocrv5-en-mobile").unwrap();
        assert_eq!(model_set.family, "PP-OCRv5");
    }

    #[test]
    fn model_registry_rejects_unknown_model_set_name() {
        assert!(model_set_by_name("missing-model-set").is_none());
    }

    #[test]
    fn all_registered_model_assets_have_sha256_checksums() {
        for model_set in available_model_sets() {
            for asset in model_set.assets() {
                assert!(
                    asset.sha256.is_some(),
                    "{} in {} is missing a SHA-256 checksum",
                    asset.filename,
                    model_set.name
                );
            }
        }
    }

    #[test]
    fn model_cache_reports_missing_assets_without_downloading() {
        let root = std::env::temp_dir().join(format!(
            "rapidocr-rs-missing-model-cache-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let cache = ModelCache::new(&root);

        let missing = cache.missing_assets(&PPOCRV6_SMALL);
        assert_eq!(missing.len(), 4);

        let err = cache
            .ensure_ppocrv6_small(ModelDownloadMode::Never)
            .unwrap_err();
        let message = format!("{err:#}");
        let _ = fs::remove_dir_all(&root);

        assert!(message.contains("failed to prepare PP-OCRv6 small detection model"));
        assert!(message.contains("missing model asset"));
    }

    #[cfg(not(feature = "model-download"))]
    #[test]
    fn model_cache_explains_when_download_feature_is_disabled() {
        let root = std::env::temp_dir().join(format!(
            "rapidocr-rs-disabled-download-cache-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let cache = ModelCache::new(&root);

        let err = cache
            .ensure_model_set_for_pipeline(
                &PPOCRV6_SMALL,
                PipelineConfig::detection_only(),
                ModelDownloadMode::Missing,
            )
            .unwrap_err();
        let message = format!("{err:#}");
        let _ = fs::remove_dir_all(&root);

        assert!(message.contains("without the `model-download` feature"));
    }
}
