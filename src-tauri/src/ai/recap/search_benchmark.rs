//! Ranking parity and an opt-in benchmark using synthetic activities only.
use super::*;

fn fixture(activities: usize) -> Corpus {
    let mut documents = Vec::with_capacity(activities);
    let mut vectors = HashMap::new();
    for i in 0..activities {
        let date = chrono::NaiveDate::from_ymd_opt(2020, 1, 1).unwrap()
            + chrono::Duration::days((i % 3650) as i64);
        let activity = RecapActivity {
            id: format!("activity-{i}"),
            task_id: format!("topic-{}", i % 50),
            task_title: format!("回顾搜索 Cache topic {}", i % 50),
            text: format!("比较 retrieval 查询缓存，完成合成记录 {i} 的实验。"),
            details: vec![
                RecapDetail {
                    text: format!("查询耗时测量结果 sample {i}"),
                    evidence_ids: vec![i as i64],
                },
                RecapDetail {
                    text: format!("讨论内存预算 database {i}"),
                    evidence_ids: vec![i as i64],
                },
            ],
            start_ms: i as i64 / 3,
            end_ms: i as i64 / 3 + 1,
            segments: vec![],
            sources: vec![],
            member_ids: vec![],
        };
        let document = Document::new(date.to_string(), 0, 0, activity);
        for chunk in &document.chunks {
            vectors.entry(chunk.key.clone()).or_insert_with(|| {
                let mut seed = i as u32 + 1;
                let mut vector = (0..MINILM_DIMENSIONS)
                    .map(|_| {
                        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                        (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.45
                    })
                    .collect::<Vec<_>>();
                let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
                for v in &mut vector {
                    *v /= norm;
                }
                vector
            });
        }
        documents.push(document);
    }
    Corpus {
        identity: Identity {
            generation: 0,
            epoch: 0,
            privacy: String::new(),
            serial: 0,
            session: None,
        },
        total_chunks: vectors.len(),
        dates: activities.min(3650),
        documents,
        vectors: Mutex::new(vectors),
    }
}

fn hit_ids(hits: &[RecapSearchHit]) -> Vec<(&str, Option<usize>, Option<usize>)> {
    hits.iter()
        .map(|h| (h.activity_id.as_str(), h.keyword_rank, h.semantic_rank))
        .collect()
}

#[test]
fn optimized_ranking_matches_the_reference_across_filters_ties_and_pages() {
    let corpus = fixture(450);
    let production = fixture(450);
    let stored = corpus.vectors.lock().unwrap();
    let baseline: BTreeMap<_, _> = stored
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_slice()))
        .collect();
    let embedding = vec![1.0 / (MINILM_DIMENSIONS as f32).sqrt(); MINILM_DIMENSIONS];
    for mode in [
        SearchMode::Keyword,
        SearchMode::Semantic,
        SearchMode::Hybrid,
    ] {
        for include_details in [true, false] {
            for offset in [0, 19, 199, 200, 449] {
                let query = RecapSearchQuery {
                    query: "cache 查询".into(),
                    mode,
                    include_details,
                    start_date: Some("2020-02-01".into()),
                    end_date: Some("2020-12-01".into()),
                    offset,
                    limit: 20,
                    ..Default::default()
                };
                let vector = (mode != SearchMode::Keyword).then_some(embedding.as_slice());
                let expected = rank_reference(&baseline, &corpus, &query, vector);
                // Reference rows borrow the map, so production readers use a
                // second fixture with identical content and vectors.
                let actual = rank(&production, &query, vector);
                assert_eq!(expected.1, actual.1);
                assert_eq!(hit_ids(&expected.0), hit_ids(&actual.0));
            }
        }
    }
}

#[test]
#[ignore = "synthetic scale benchmark; run with --ignored --nocapture"]
fn measure_recap_search_scale() {
    let sizes = std::env::var("CARBONPAPER_RECAP_BENCH_SIZES")
        .unwrap_or_else(|_| "10000,50000,100000".into());
    let samples = std::env::var("CARBONPAPER_RECAP_BENCH_SAMPLES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(7usize);
    let mut report = vec![];
    for size in sizes.split(',').map(|s| s.parse::<usize>().unwrap()) {
        let corpus = fixture(size);
        let stored = corpus.vectors.lock().unwrap();
        let baseline: BTreeMap<_, _> = stored
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_slice()))
            .collect();
        let mut baseline_ms = vec![];
        let query = RecapSearchQuery {
            query: "cache 查询".into(),
            ..Default::default()
        };
        let embedding = vec![1.0 / (MINILM_DIMENSIONS as f32).sqrt(); MINILM_DIMENSIONS];
        for _ in 0..samples {
            let start = Instant::now();
            std::hint::black_box(rank_reference(&baseline, &corpus, &query, Some(&embedding)));
            baseline_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        drop(baseline);
        drop(stored);
        let mut optimized_ms = vec![];
        for _ in 0..samples {
            let start = Instant::now();
            std::hint::black_box(rank(&corpus, &query, Some(&embedding)));
            optimized_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        baseline_ms.sort_by(f64::total_cmp);
        optimized_ms.sort_by(f64::total_cmp);
        let percentile = |rows: &[f64], p: f64| {
            rows[((rows.len() as f64 * p).ceil() as usize).saturating_sub(1)]
        };
        let row = serde_json::json!({ "activities":size, "chunks":corpus.total_chunks, "samples":samples,
            "vector_bytes":corpus.total_chunks * MINILM_DIMENSIONS * 4,
            "baseline_p50_ms":percentile(&baseline_ms,0.5), "baseline_p95_ms":percentile(&baseline_ms,0.95),
            "optimized_p50_ms":percentile(&optimized_ms,0.5), "optimized_p95_ms":percentile(&optimized_ms,0.95),
            "scope":"resident hybrid ranking, synthetic records, excludes DB loading, query encoding and UI" });
        println!("{row}");
        report.push(row);
    }
    if let Ok(path) = std::env::var("CARBONPAPER_RECAP_BENCH_OUTPUT") {
        let path = std::path::Path::new(&path);
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}
fn rank_reference(
    baseline: &BTreeMap<&str, &[f32]>,
    corpus: &Corpus,
    query: &RecapSearchQuery,
    embedding: Option<&[f32]>,
) -> (Vec<RecapSearchHit>, usize, usize) {
    let mut keywords = Vec::new();
    let mut semantic = Vec::new();
    let vectors = baseline;
    for (i, document) in corpus
        .documents
        .iter()
        .enumerate()
        .filter(|(_, d)| query.includes(&d.date))
    {
        if query.mode != SearchMode::Semantic {
            let score = lexical_evidence(
                &document.activity,
                &QueryPlan::build(&query.query),
                query.include_details,
            )
            .map(|e| e.relevance_score)
            .unwrap_or_default();
            if score > 0.0 {
                keywords.push((i, score));
            }
        }
        if let Some(embedding) = embedding {
            let score = document
                .chunks
                .iter()
                .filter(|c| query.include_details || !c.detail)
                .filter_map(|c| vectors.get(c.key.as_str()))
                .map(|v| {
                    v.iter()
                        .zip(embedding)
                        .map(|(a, b)| f64::from(a * b))
                        .sum::<f64>()
                })
                .filter(|s| s.is_finite() && *s >= MIN_SEMANTIC_SCORE)
                .max_by(f64::total_cmp);
            if let Some(score) = score {
                semantic.push((i, score));
            }
        }
    }
    let indexed = vectors.len();

    let sort = |rows: &mut Vec<(usize, f64)>| {
        rows.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then_with(|| {
                    corpus.documents[b.0]
                        .activity
                        .start_ms
                        .cmp(&corpus.documents[a.0].activity.start_ms)
                })
                .then_with(|| a.0.cmp(&b.0))
        })
    };
    sort(&mut keywords);
    sort(&mut semantic);
    let mut ranks: HashMap<usize, (Option<usize>, Option<usize>)> = HashMap::new();
    for (position, (i, _)) in keywords.iter().enumerate() {
        ranks.entry(*i).or_default().0 = Some(position + 1);
    }
    // Bound broad semantic tails while preserving all exact keyword hits.
    for (position, (i, _)) in semantic.iter().take(200).enumerate() {
        ranks.entry(*i).or_default().1 = Some(position + 1);
    }
    let mut ranked: Vec<_> = ranks.into_iter().collect();
    let rrf = |ranks: &(Option<usize>, Option<usize>)| {
        [ranks.0, ranks.1]
            .into_iter()
            .flatten()
            .map(|rank| 1.0 / (60 + rank) as f64)
            .sum::<f64>()
    };
    ranked.sort_by(|(a, ar), (b, br)| {
        rrf(br)
            .total_cmp(&rrf(ar))
            .then_with(|| {
                corpus.documents[*b]
                    .activity
                    .start_ms
                    .cmp(&corpus.documents[*a].activity.start_ms)
            })
            .then(a.cmp(b))
    });
    let total = ranked.len();
    let hits = ranked
        .into_iter()
        .skip(query.offset)
        .take(query.limit)
        .map(|(i, (keyword_rank, semantic_rank))| {
            let d = &corpus.documents[i];
            let a = &d.activity;
            RecapSearchHit {
                result_offset: None,
                date: d.date.clone(),
                batch_start_ms: d.batch_start_ms,
                activity_id: a.id.clone(),
                task_id: a.task_id.clone(),
                task_title: a.task_title.clone(),
                text: a.text.clone(),
                details: a.details.clone(),
                start_ms: a.start_ms,
                end_ms: a.end_ms,
                sources: a.sources.clone(),
                evidence: d.evidence.clone(),
                keyword_rank,
                semantic_rank,
                keyword_score: None,
                semantic_score: None,
                match_evidence: None,
            }
        })
        .collect();
    (hits, total, indexed)
}
