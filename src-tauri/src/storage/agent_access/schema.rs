//! Source projection queries and encrypted transactional outbox triggers.
use super::{db, Connection};
use rusqlite::types::Value as SqlValue;

const PAGE_SIZE: i64 = 128;

#[derive(Clone, Copy)]
enum SourceKey {
    Integer(&'static str),
    Text(&'static str),
    IntegerPair(&'static str, &'static str),
    TextPair(&'static str, &'static str),
}

pub(super) struct Source {
    pub kind: &'static str,
    key: SourceKey,
    from: &'static str,
    visible: &'static str,
    projection: &'static str,
}

impl Source {
    pub fn query(
        &self,
        cursor: &str,
        key: Option<&str>,
    ) -> Result<(String, [SqlValue; 3]), String> {
        let exact = key.is_some();
        let value = key.unwrap_or(cursor);
        let integer = |value: &str| -> Result<SqlValue, String> {
            if value.is_empty() && !exact {
                return Ok(SqlValue::Integer(0));
            }
            value
                .parse::<i64>()
                .map(SqlValue::Integer)
                .map_err(|_| "AGENT_INVALID_SOURCE_KEY".into())
        };
        let pair = || -> Result<(&str, &str), String> {
            if value.is_empty() && !exact {
                return Ok(("", ""));
            }
            value
                .split_once(':')
                .ok_or_else(|| "AGENT_INVALID_SOURCE_KEY".into())
        };
        let text = |value: &str| SqlValue::Text(value.into());
        let (first, second, parameters) = match self.key {
            SourceKey::Integer(column) => (
                column,
                "NULL",
                [
                    integer(value)?,
                    SqlValue::Null,
                    SqlValue::Integer(PAGE_SIZE),
                ],
            ),
            SourceKey::Text(column) => (
                column,
                "NULL",
                [text(value), SqlValue::Null, SqlValue::Integer(PAGE_SIZE)],
            ),
            SourceKey::IntegerPair(first, second) => {
                let (a, b) = pair()?;
                (
                    first,
                    second,
                    [integer(a)?, integer(b)?, SqlValue::Integer(PAGE_SIZE)],
                )
            }
            SourceKey::TextPair(first, second) => {
                let (a, b) = pair()?;
                (
                    first,
                    second,
                    [text(a), text(b), SqlValue::Integer(PAGE_SIZE)],
                )
            }
        };
        let page = {
            let operator = if exact { "=" } else { ">" };
            let predicate = if second == "NULL" {
                format!("{first}{operator}?1")
            } else {
                format!("({first},{second}){operator}(?1,?2)")
            };
            format!("SELECT {first} k1,{second} k2 FROM {} WHERE {} AND {predicate} ORDER BY {first},{second} LIMIT ?3", self.from, self.visible)
        };
        // Select only indexed native keys before constructing JSON, copying
        // encrypted blobs or calling DIRECTONLY plaintext projection functions.
        Ok((
            format!(
                "WITH page AS MATERIALIZED ({page}) {} ORDER BY page.k1,page.k2",
                self.projection
            ),
            parameters,
        ))
    }
}

pub(super) const SOURCES: &[Source] = &[
    Source {
        kind: "snapshot", key: SourceKey::Integer("s.id"),
        from: "screenshots s NOT INDEXED", visible: "s.is_deleted=0",
        projection: "SELECT CAST(s.id AS TEXT) source_key,json_object('screenshot_id',s.id,'captured_at_ms',CAST(strftime('%s',s.created_at) AS INTEGER)*1000,
        'width',s.width,'height',s.height,'window_title',s.window_title,'process_name',cp_process(s.process_name),'source',s.source,'page_url',NULL,
        'category',s.category,'status',s.status,'window_title_enc',hex(s.window_title_enc),'process_name_enc',hex(s.process_name_enc),'page_url_enc',hex(s.page_url_enc),
        'key_enc',hex(s.content_key_encrypted),'links_enc',hex(l.links_enc),'links_key',hex(l.links_key_encrypted)) payload FROM page CROSS JOIN screenshots s ON s.id=page.k1 LEFT JOIN link_sets l ON l.id=s.link_set_id",
    },
    Source {
        kind: "ocr", key: SourceKey::Integer("o.id"),
        from: "ocr_results o NOT INDEXED CROSS JOIN screenshots s ON s.id=o.screenshot_id",
        visible: "o.is_deleted=0 AND s.is_deleted=0",
        projection: "SELECT CAST(o.id AS TEXT) source_key,json_object('ocr_id',o.id,'screenshot_id',o.screenshot_id,
        'payload_digest',cp_agent_digest(o.text_enc),'key_enc',hex(o.text_key_encrypted)) payload FROM page CROSS JOIN ocr_results o ON o.id=page.k1",
    },
    Source {
        kind: "cluster", key: SourceKey::Integer("c.id"),
        from: "smart_clusters c", visible: "1=1",
        projection: "SELECT CAST(c.id AS TEXT) source_key,json_object('cluster_id',c.id,'display_name',cp_text(c.display_name,'cluster_name:'||c.id),'anchor_text',cp_text(c.anchor_text,'cluster_anchor:'||c.id),
        'enabled',json(CASE c.enabled WHEN 1 THEN 'true' ELSE 'false' END),'created_at_ms',CAST(strftime('%s',c.created_at) AS INTEGER)*1000,'updated_at_ms',CAST(strftime('%s',c.updated_at) AS INTEGER)*1000,
        'snapshot_count',(SELECT COUNT(*) FROM smart_cluster_assignments WHERE smart_cluster_id=c.id)) payload FROM page CROSS JOIN smart_clusters c ON c.id=page.k1",
    },
    Source {
        kind: "summary", key: SourceKey::Integer("c.smart_cluster_id"),
        from: "smart_cluster_summaries c", visible: "1=1",
        projection: "SELECT CAST(c.smart_cluster_id AS TEXT) source_key,json_object('cluster_id',c.smart_cluster_id,
        'title',cp_text(c.title,'cluster_summary:'||c.smart_cluster_id||':title'),'summary',cp_text(c.summary,'cluster_summary:'||c.smart_cluster_id||':summary'),
        'ocr_summary',cp_text(c.ocr_summary,'cluster_summary:'||c.smart_cluster_id||':ocr_summary'),'key_points_json',cp_text(c.key_points_json,'cluster_summary:'||c.smart_cluster_id||':key_points_json'),
        'evidence_json',cp_text(c.evidence_json,'cluster_summary:'||c.smart_cluster_id||':evidence_json'),'source_snapshot_count',c.source_snapshot_count,
        'created_at_ms',CAST(strftime('%s',c.created_at) AS INTEGER)*1000,'updated_at_ms',CAST(strftime('%s',c.updated_at) AS INTEGER)*1000) payload FROM page CROSS JOIN smart_cluster_summaries c ON c.smart_cluster_id=page.k1",
    },
    Source {
        kind: "assignment", key: SourceKey::IntegerPair("a.smart_cluster_id", "a.screenshot_id"),
        from: "smart_cluster_assignments a CROSS JOIN screenshots s ON s.id=a.screenshot_id", visible: "s.is_deleted=0",
        projection: "SELECT a.smart_cluster_id||':'||a.screenshot_id source_key,json_object('cluster_id',a.smart_cluster_id,'screenshot_id',a.screenshot_id,'rerank_score',a.rerank_score,
        'assigned_at_ms',CAST(strftime('%s',a.assigned_at) AS INTEGER)*1000) payload FROM page CROSS JOIN smart_cluster_assignments a ON (a.smart_cluster_id,a.screenshot_id)=(page.k1,page.k2)",
    },
    Source {
        kind: "calendar", key: SourceKey::Text("d.day"),
        from: "recap_days d", visible: "NOT EXISTS(SELECT 1 FROM recap_deleted_days x WHERE x.day=d.day)",
        projection: "SELECT d.day source_key,json_set(COALESCE(c.data,json_object('date',d.day,'rule','recorded_bounds','start_ms',d.start_ms,'end_ms',d.end_ms,'start_utc_offset_seconds',0,'end_utc_offset_seconds',0,'period_boundaries_ms',json_array())),
        '$.source_revision',d.revision) payload FROM page CROSS JOIN recap_days d ON d.day=page.k1 LEFT JOIN recap_calendars c ON c.day=d.day",
    },
    Source {
        kind: "recap", key: SourceKey::TextPair("p.kind", "p.key"),
        from: "recap_payloads p CROSS JOIN recap_days d ON d.day=p.day",
        visible: "p.kind IN ('batch','summary','corrections','threads','search') AND (p.kind IN ('corrections','threads') OR d.revision=p.revision) AND NOT EXISTS(SELECT 1 FROM recap_deleted_days x WHERE x.day=p.day)",
        projection: "SELECT p.kind||':'||p.key source_key,json_object('date',p.day,'key',p.key,'part',p.kind,'revision',p.revision,'content',NULL,
            'data_enc',CASE WHEN p.kind='search' THEN NULL ELSE hex(p.data) END,'payload_digest',cp_agent_digest(p.data),'key_enc',hex(p.row_key)) payload
        FROM page CROSS JOIN recap_payloads p ON (p.kind,p.key)=(page.k1,page.k2)",
    },
    Source {
        kind: "vector_key", key: SourceKey::Text("k.key_id"), from: "protected_storage_keys k", visible: "k.purpose='vectors'",
        projection: "SELECT k.key_id source_key,json_object('key_id',k.key_id,'key_enc',hex(k.wrapped_key)) payload
            FROM page CROSS JOIN protected_storage_keys k ON k.key_id=page.k1",
    },
];

pub(super) fn install_triggers(conn: &Connection) -> Result<(), String> {
    let triggers:[(&str,&str,&str);15]=[
        ("snapshot_insert","AFTER INSERT ON screenshots","SELECT cp_agent_emit('snapshot',CAST(NEW.id AS TEXT),'put',json_object('screenshot_id',NEW.id,'captured_at_ms',CAST(strftime('%s',NEW.created_at) AS INTEGER)*1000,
            'width',NEW.width,'height',NEW.height,'window_title',NEW.window_title,'process_name',NEW.process_name,'source',NEW.source,'page_url',NULL,'category',NEW.category,'status',NEW.status,
            'encrypted',json(CASE WHEN NEW.window_title_enc IS NOT NULL OR NEW.process_name_enc IS NOT NULL OR NEW.page_url_enc IS NOT NULL OR NEW.link_set_id IS NOT NULL THEN 'true' ELSE 'false' END)))"),
        ("snapshot_update","AFTER UPDATE OF category,status,is_deleted,window_title_enc,process_name_enc,page_url_enc,link_set_id,width,height,created_at ON screenshots","SELECT cp_agent_emit('snapshot_state',CAST(NEW.id AS TEXT),'put',json_object('screenshot_id',NEW.id,'category',NEW.category,'status',NEW.status,'deleted',json(CASE NEW.is_deleted WHEN 1 THEN 'true' ELSE 'false' END),
            'requires_projection',json(CASE WHEN NEW.is_deleted=0 AND (NEW.window_title_enc IS NOT OLD.window_title_enc OR NEW.process_name_enc IS NOT OLD.process_name_enc OR NEW.page_url_enc IS NOT OLD.page_url_enc OR NEW.link_set_id IS NOT OLD.link_set_id OR NEW.width IS NOT OLD.width OR NEW.height IS NOT OLD.height OR NEW.created_at IS NOT OLD.created_at OR OLD.is_deleted=1) THEN 'true' ELSE 'false' END)))"),
        ("snapshot_delete","AFTER DELETE ON screenshots","SELECT cp_agent_emit('snapshot',CAST(OLD.id AS TEXT),'delete','{}')"),
        ("ocr_insert","AFTER INSERT ON ocr_results","SELECT cp_agent_emit('ocr',CAST(NEW.id AS TEXT),'put',json_object('ocr_id',NEW.id,'screenshot_id',NEW.screenshot_id,'payload_digest',cp_agent_digest(NEW.text_enc)))"),
        ("ocr_update","AFTER UPDATE OF text_enc,text_key_encrypted,is_deleted,screenshot_id ON ocr_results","SELECT cp_agent_emit('ocr',CAST(NEW.id AS TEXT),CASE NEW.is_deleted WHEN 1 THEN 'delete' ELSE 'put' END,json_object('ocr_id',NEW.id,'screenshot_id',NEW.screenshot_id,'payload_digest',cp_agent_digest(NEW.text_enc)))"),
        ("ocr_delete","AFTER DELETE ON ocr_results","SELECT cp_agent_emit('ocr',CAST(OLD.id AS TEXT),'delete','{}')"),
        ("cluster_delete","AFTER DELETE ON smart_clusters","SELECT cp_agent_emit('cluster',CAST(OLD.id AS TEXT),'delete','{}')"),
        ("summary_delete","AFTER DELETE ON smart_cluster_summaries","SELECT cp_agent_emit('summary',CAST(OLD.smart_cluster_id AS TEXT),'delete','{}')"),
        ("assignment_delete","AFTER DELETE ON smart_cluster_assignments","SELECT cp_agent_emit('assignment',OLD.smart_cluster_id||':'||OLD.screenshot_id,'delete','{}')"),
        ("recap_delete","AFTER DELETE ON recap_payloads WHEN OLD.kind IN ('batch','summary','corrections','threads','search')","SELECT cp_agent_emit('recap',OLD.kind||':'||OLD.key,'delete','{}')"),
        ("day_delete","AFTER DELETE ON recap_days","SELECT cp_agent_emit('day_delete',OLD.day,'put','{}')"),
        ("day_privacy_delete","AFTER INSERT ON recap_deleted_days","SELECT cp_agent_emit('day_delete',NEW.day,'put','{}')"),
        ("calendar_delete","AFTER DELETE ON recap_calendars","SELECT cp_agent_emit('calendar',OLD.day,'delete','{}')"),
        ("vector_delete","AFTER DELETE ON derived_embeddings WHEN OLD.index_kind IN ('clip_image','semantic_text')","SELECT cp_agent_emit('source','vectors','put','{}')"),
        ("day_restore","AFTER DELETE ON recap_deleted_days","SELECT cp_agent_emit('calendar',OLD.day,'put',json_set(COALESCE((SELECT data FROM recap_calendars WHERE day=OLD.day),json_object('date',OLD.day,'rule','recorded_bounds','start_ms',0,'end_ms',0,'start_utc_offset_seconds',0,'end_utc_offset_seconds',0,'period_boundaries_ms',json_array())),'$.source_revision',COALESCE((SELECT revision FROM recap_days WHERE day=OLD.day),0)))"),
    ];
    for (name, timing, select) in triggers {
        let timing = timing.replace(" WHEN ", " WHEN cp_agent_enabled() AND ");
        let timing = if timing.contains(" WHEN ") {
            timing
        } else {
            format!("{timing} WHEN cp_agent_enabled()")
        };
        db(conn.execute_batch(&format!("DROP TRIGGER IF EXISTS agent_{name};CREATE TRIGGER agent_{name} {timing} BEGIN INSERT INTO agent_enqueue(envelope) {select};END;")))?;
    }
    let puts=[
        ("cluster","smart_clusters","CAST(NEW.id AS TEXT)","json_object('cluster_id',NEW.id,'anchor_text',NEW.anchor_text,'display_name',NEW.display_name,'missing_name',json(CASE WHEN NEW.display_name IS NOT NULL AND NEW.display_name IS NULL THEN 'true' ELSE 'false' END),'enabled',json(CASE NEW.enabled WHEN 1 THEN 'true' ELSE 'false' END),'created_at_ms',CAST(strftime('%s',NEW.created_at) AS INTEGER)*1000,'updated_at_ms',CAST(strftime('%s',NEW.updated_at) AS INTEGER)*1000,'snapshot_count',0)"),
        ("summary","smart_cluster_summaries","CAST(NEW.smart_cluster_id AS TEXT)","json_object('cluster_id',NEW.smart_cluster_id,'title',NEW.title,'summary',NEW.summary,'ocr_summary',NEW.ocr_summary,'key_points_json',NEW.key_points_json,'evidence_json',NEW.evidence_json,'source_snapshot_count',NEW.source_snapshot_count,'created_at_ms',CAST(strftime('%s',NEW.created_at) AS INTEGER)*1000,'updated_at_ms',CAST(strftime('%s',NEW.updated_at) AS INTEGER)*1000,'unavailable',json(CASE WHEN (NEW.title IS NOT NULL AND NEW.title IS NULL) OR (NEW.summary IS NOT NULL AND NEW.summary IS NULL) OR (NEW.ocr_summary IS NOT NULL AND NEW.ocr_summary IS NULL) OR (NEW.key_points_json IS NOT NULL AND NEW.key_points_json IS NULL) OR (NEW.evidence_json IS NOT NULL AND NEW.evidence_json IS NULL) THEN 'true' ELSE 'false' END))"),
        ("assignment","smart_cluster_assignments","NEW.smart_cluster_id||':'||NEW.screenshot_id","json_object('cluster_id',NEW.smart_cluster_id,'screenshot_id',NEW.screenshot_id,'rerank_score',NEW.rerank_score,'assigned_at_ms',CAST(strftime('%s',NEW.assigned_at) AS INTEGER)*1000)"),
        ("recap","recap_payloads","NEW.kind||':'||NEW.key","json_object('date',NEW.day,'key',NEW.key,'part',NEW.kind,'revision',NEW.revision,'payload_digest',cp_agent_digest(NEW.data))"),
        ("calendar","recap_calendars","NEW.day","json_set(NEW.data,'$.source_revision',COALESCE((SELECT revision FROM recap_days WHERE day=NEW.day),0))"),
    ];
    for (kind, table, key, payload) in puts {
        for operation in ["INSERT", "UPDATE"] {
            let condition = if kind == "recap" {
                " AND NEW.kind IN ('batch','summary','corrections','threads','search')"
            } else {
                ""
            };
            let columns = if kind == "cluster" && operation == "UPDATE" {
                " OF anchor_text,display_name,enabled"
            } else {
                ""
            };
            db(conn.execute_batch(&format!("DROP TRIGGER IF EXISTS agent_{kind}_{operation};CREATE TRIGGER agent_{kind}_{operation} AFTER {operation}{columns} ON {table} WHEN cp_agent_enabled(){condition}
            BEGIN INSERT INTO agent_enqueue(envelope) SELECT cp_agent_emit('{kind}',{key},'put',{payload});END;")))?;
        }
    }
    for operation in ["INSERT", "UPDATE"] {
        db(conn.execute_batch(&format!("DROP TRIGGER IF EXISTS agent_vector_{operation};CREATE TRIGGER agent_vector_{operation} AFTER {operation} ON derived_embeddings WHEN cp_agent_enabled() AND NEW.index_kind IN ('clip_image','semantic_text')
            BEGIN INSERT INTO agent_enqueue(envelope) SELECT cp_agent_emit('vector_key',lower(hex(substr(NEW.vector_f32,9,16))),'put',json_object('key_id',lower(hex(substr(NEW.vector_f32,9,16)))));END;
            DROP TRIGGER IF EXISTS agent_day_{operation};CREATE TRIGGER agent_day_{operation} AFTER {operation} ON recap_days WHEN cp_agent_enabled()
            BEGIN INSERT INTO agent_enqueue(envelope) SELECT cp_agent_emit('calendar',NEW.day,'put',json_set(COALESCE((SELECT data FROM recap_calendars WHERE day=NEW.day),json_object('date',NEW.day,'rule','recorded_bounds','start_ms',NEW.start_ms,'end_ms',NEW.end_ms,'start_utc_offset_seconds',0,'end_utc_offset_seconds',0,'period_boundaries_ms',json_array())),'$.source_revision',NEW.revision));END;")))?;
    }
    Ok(())
}
