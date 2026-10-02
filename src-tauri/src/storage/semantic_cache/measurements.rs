//! Opt-in local measurement. Run with --release --ignored --nocapture.
use super::*;
#[test]
#[ignore = "measurement, not a regression; run with --release --ignored --nocapture"]
fn bench_scoring_a_hot_layer_sized_matrix() {
    let (rows, dimensions) = (9558usize, 384usize);
    let mut cache = SemanticVectorCache::new(1);
    for row in 0..rows {
        let vector: Vec<f32> = (0..dimensions)
            .map(|i| (((row * 31 + i * 7) % 1000) as f32) / 1000.0)
            .collect();
        assert!(cache.push(row.to_string(), vector));
    }
    let query: Vec<f32> = (0..dimensions)
        .map(|i| ((i % 997) as f32) / 997.0)
        .collect();
    let _ = cache.top_candidates(&query, 26);
    let runs = 20;
    let started = std::time::Instant::now();
    for _ in 0..runs {
        assert_eq!(cache.top_candidates(&query, 26).len(), 26);
    }
    let per_query = started.elapsed().as_secs_f64() * 1000.0 / runs as f64;
    println!(
        "top_candidates over {rows}x{dimensions}: {per_query:.3} ms/query, {} KiB resident",
        cache.allocated_bytes() / 1024
    );
}
