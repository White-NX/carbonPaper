use std::{path::PathBuf, time::Duration};

use rapidocr_core::{
    config::PipelineConfig,
    model::{model_set_by_name, ModelCache, ModelDownloadMode},
    tokio::{TokioOcrError, TokioRapidOcr},
};

fn main() -> anyhow::Result<()> {
    let image_path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("usage: tokio_usage <image-path>"))?;

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async move {
            let model_set = model_set_by_name("ppocrv5-ch-mobile").unwrap();
            let cache = ModelCache::new("models");
            let pipeline = PipelineConfig::without_cls();
            cache.ensure_model_set_for_pipeline(model_set, pipeline, ModelDownloadMode::Missing)?;

            let mut config = cache.config_for(model_set).with_pipeline(pipeline);
            config.inference.intra_threads = 2;
            let ocr = TokioRapidOcr::new(config).await?;
            match ocr
                .run_path_with_timeout(image_path, Duration::from_secs(10))
                .await
            {
                Ok(output) => println!("recognized {} lines", output.output.lines.len()),
                Err(TokioOcrError::TimedOut(duration)) => {
                    eprintln!("OCR timed out after {duration:?}")
                }
                Err(error) => return Err(error.into()),
            }
            ocr.shutdown().await?;
            Ok(())
        })
}
