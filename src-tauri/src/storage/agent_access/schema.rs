//! Source projection queries and encrypted transactional outbox triggers.
use super::{db, Connection};
pub(super) const SOURCES:&[(&str,&str)]=&[
    ("snapshot","SELECT CAST(s.id AS TEXT) source_key,json_object('screenshot_id',s.id,'captured_at_ms',CAST(strftime('%s',s.created_at) AS INTEGER)*1000,
        'width',s.width,'height',s.height,'window_title',s.window_title,'process_name',cp_process(s.process_name),'source',s.source,'page_url',NULL,
        'category',s.category,'status',s.status,'window_title_enc',hex(s.window_title_enc),'process_name_enc',hex(s.process_name_enc),'page_url_enc',hex(s.page_url_enc),
        'key_enc',hex(s.content_key_encrypted),'links_enc',hex(l.links_enc),'links_key',hex(l.links_key_encrypted)) payload FROM screenshots s LEFT JOIN link_sets l ON l.id=s.link_set_id WHERE s.is_deleted=0"),
    ("ocr","SELECT CAST(o.id AS TEXT) source_key,json_object('ocr_id',o.id,'screenshot_id',o.screenshot_id,'text',o.text,'confidence',o.confidence,
        'ocr_processed_at_ms',CAST(strftime('%s',o.created_at) AS INTEGER)*1000,'box_coords',json_array(json_array(o.box_x1,o.box_y1),json_array(o.box_x2,o.box_y2),json_array(o.box_x3,o.box_y3),json_array(o.box_x4,o.box_y4)),
        'data_enc',hex(o.text_enc),'key_enc',hex(o.text_key_encrypted)) payload FROM ocr_results o JOIN screenshots s ON s.id=o.screenshot_id WHERE o.is_deleted=0 AND s.is_deleted=0"),
    ("cluster","SELECT CAST(c.id AS TEXT) source_key,json_object('cluster_id',c.id,'display_name',cp_text(c.display_name,'cluster_name:'||c.id),'anchor_text',cp_text(c.anchor_text,'cluster_anchor:'||c.id),
        'enabled',json(CASE c.enabled WHEN 1 THEN 'true' ELSE 'false' END),'created_at_ms',CAST(strftime('%s',c.created_at) AS INTEGER)*1000,'updated_at_ms',CAST(strftime('%s',c.updated_at) AS INTEGER)*1000,
        'snapshot_count',(SELECT COUNT(*) FROM smart_cluster_assignments WHERE smart_cluster_id=c.id)) payload FROM smart_clusters c"),
    ("summary","SELECT CAST(c.smart_cluster_id AS TEXT) source_key,json_object('cluster_id',c.smart_cluster_id,
        'title',cp_text(c.title,'cluster_summary:'||c.smart_cluster_id||':title'),'summary',cp_text(c.summary,'cluster_summary:'||c.smart_cluster_id||':summary'),
        'ocr_summary',cp_text(c.ocr_summary,'cluster_summary:'||c.smart_cluster_id||':ocr_summary'),'key_points_json',cp_text(c.key_points_json,'cluster_summary:'||c.smart_cluster_id||':key_points_json'),
        'evidence_json',cp_text(c.evidence_json,'cluster_summary:'||c.smart_cluster_id||':evidence_json'),'source_snapshot_count',c.source_snapshot_count,
        'created_at_ms',CAST(strftime('%s',c.created_at) AS INTEGER)*1000,'updated_at_ms',CAST(strftime('%s',c.updated_at) AS INTEGER)*1000) payload FROM smart_cluster_summaries c"),
    ("assignment","SELECT a.smart_cluster_id||':'||a.screenshot_id source_key,json_object('cluster_id',a.smart_cluster_id,'screenshot_id',a.screenshot_id,'rerank_score',a.rerank_score,
        'assigned_at_ms',CAST(strftime('%s',a.assigned_at) AS INTEGER)*1000) payload FROM smart_cluster_assignments a JOIN screenshots s ON s.id=a.screenshot_id WHERE s.is_deleted=0"),
    ("calendar","SELECT d.day source_key,json_set(COALESCE(c.data,json_object('date',d.day,'rule','recorded_bounds','start_ms',d.start_ms,'end_ms',d.end_ms,'start_utc_offset_seconds',0,'end_utc_offset_seconds',0,'period_boundaries_ms',json_array())),
        '$.source_revision',d.revision) payload FROM recap_days d LEFT JOIN recap_calendars c ON c.day=d.day WHERE NOT EXISTS(SELECT 1 FROM recap_deleted_days x WHERE x.day=d.day)"),
    ("recap","SELECT p.kind||':'||p.key source_key,json_object('date',p.day,'key',p.key,'part',p.kind,'revision',p.revision,'content',NULL,'data_enc',hex(p.data),'key_enc',hex(p.row_key)) payload
        FROM recap_payloads p JOIN recap_days d ON d.day=p.day WHERE p.kind IN ('batch','summary','corrections','threads','search') AND (p.kind IN ('corrections','threads') OR d.revision=p.revision) AND NOT EXISTS(SELECT 1 FROM recap_deleted_days x WHERE x.day=p.day)"),
    ("vector","SELECT e.index_kind||':'||s.id source_key,json_object('kind',e.index_kind,'screenshot_id',s.id,'model_id',e.model_id,'model_revision',e.model_revision,
        'vector_hex',hex(cp_unseal(e.vector_f32,'vectors',e.index_kind||':'||e.subject_key))) payload FROM derived_embeddings e JOIN screenshots s
        ON (e.index_kind='clip_image' AND s.image_hash=e.subject_key) OR (e.index_kind='semantic_text' AND CAST(s.id AS TEXT)=e.subject_key)
        WHERE s.is_deleted=0 AND e.index_kind IN ('clip_image','semantic_text')"),
];

pub(super) fn install_triggers(conn: &Connection) -> Result<(), String> {
    let triggers:[(&str,&str,&str);15]=[
        ("snapshot_insert","AFTER INSERT ON screenshots","SELECT cp_agent_emit('snapshot',CAST(NEW.id AS TEXT),'put',json_object('screenshot_id',NEW.id,'captured_at_ms',CAST(strftime('%s',NEW.created_at) AS INTEGER)*1000,
            'width',NEW.width,'height',NEW.height,'window_title',NEW.window_title,'process_name',NEW.process_name,'source',NEW.source,'page_url',NULL,'category',NEW.category,'status',NEW.status,
            'encrypted',json(CASE WHEN NEW.window_title_enc IS NOT NULL OR NEW.process_name_enc IS NOT NULL OR NEW.page_url_enc IS NOT NULL OR NEW.link_set_id IS NOT NULL THEN 'true' ELSE 'false' END)))"),
        ("snapshot_update","AFTER UPDATE OF category,status,is_deleted,window_title_enc,process_name_enc,page_url_enc,link_set_id,width,height,created_at ON screenshots","SELECT cp_agent_emit('snapshot_state',CAST(NEW.id AS TEXT),'put',json_object('screenshot_id',NEW.id,'category',NEW.category,'status',NEW.status,'deleted',json(CASE NEW.is_deleted WHEN 1 THEN 'true' ELSE 'false' END),
            'requires_projection',json(CASE WHEN NEW.is_deleted=0 AND (NEW.window_title_enc IS NOT OLD.window_title_enc OR NEW.process_name_enc IS NOT OLD.process_name_enc OR NEW.page_url_enc IS NOT OLD.page_url_enc OR NEW.link_set_id IS NOT OLD.link_set_id OR NEW.width IS NOT OLD.width OR NEW.height IS NOT OLD.height OR NEW.created_at IS NOT OLD.created_at OR OLD.is_deleted=1) THEN 'true' ELSE 'false' END)))"),
        ("snapshot_delete","AFTER DELETE ON screenshots","SELECT cp_agent_emit('snapshot',CAST(OLD.id AS TEXT),'delete','{}')"),
        ("ocr_insert","AFTER INSERT ON ocr_results","SELECT cp_agent_emit('ocr',CAST(NEW.id AS TEXT),'put',json_object('ocr_id',NEW.id,'screenshot_id',NEW.screenshot_id,'text',NEW.text,'text_hex',hex(NEW.text_enc),'confidence',NEW.confidence,
            'box_coords',json_array(json_array(NEW.box_x1,NEW.box_y1),json_array(NEW.box_x2,NEW.box_y2),json_array(NEW.box_x3,NEW.box_y3),json_array(NEW.box_x4,NEW.box_y4)),'ocr_processed_at_ms',CAST(strftime('%s',NEW.created_at) AS INTEGER)*1000))"),
        ("ocr_update","AFTER UPDATE OF text_enc,text,confidence,is_deleted,box_x1,box_y1,box_x2,box_y2,box_x3,box_y3,box_x4,box_y4 ON ocr_results","SELECT cp_agent_emit('ocr',CAST(NEW.id AS TEXT),CASE NEW.is_deleted WHEN 1 THEN 'delete' ELSE 'put' END,json_object('ocr_id',NEW.id,'screenshot_id',NEW.screenshot_id,'text',NEW.text,'text_hex',hex(NEW.text_enc),'confidence',NEW.confidence,
            'box_coords',json_array(json_array(NEW.box_x1,NEW.box_y1),json_array(NEW.box_x2,NEW.box_y2),json_array(NEW.box_x3,NEW.box_y3),json_array(NEW.box_x4,NEW.box_y4)),'ocr_processed_at_ms',CAST(strftime('%s',NEW.created_at) AS INTEGER)*1000))"),
        ("ocr_delete","AFTER DELETE ON ocr_results","SELECT cp_agent_emit('ocr',CAST(OLD.id AS TEXT),'delete','{}')"),
        ("cluster_delete","AFTER DELETE ON smart_clusters","SELECT cp_agent_emit('cluster',CAST(OLD.id AS TEXT),'delete','{}')"),
        ("summary_delete","AFTER DELETE ON smart_cluster_summaries","SELECT cp_agent_emit('summary',CAST(OLD.smart_cluster_id AS TEXT),'delete','{}')"),
        ("assignment_delete","AFTER DELETE ON smart_cluster_assignments","SELECT cp_agent_emit('assignment',OLD.smart_cluster_id||':'||OLD.screenshot_id,'delete','{}')"),
        ("recap_delete","AFTER DELETE ON recap_payloads WHEN OLD.kind IN ('batch','summary','corrections','threads','search')","SELECT cp_agent_emit('recap',OLD.kind||':'||OLD.key,'delete','{}')"),
        ("day_delete","AFTER DELETE ON recap_days","SELECT cp_agent_emit('day_delete',OLD.day,'put','{}')"),
        ("day_privacy_delete","AFTER INSERT ON recap_deleted_days","SELECT cp_agent_emit('day_delete',NEW.day,'put','{}')"),
        ("calendar_delete","AFTER DELETE ON recap_calendars","SELECT cp_agent_emit('calendar',OLD.day,'delete','{}')"),
        ("vector_delete","AFTER DELETE ON derived_embeddings","SELECT cp_agent_emit('vector',OLD.index_kind||':'||s.id,'delete','{}') FROM screenshots s WHERE (OLD.index_kind='clip_image' AND s.image_hash=OLD.subject_key) OR (OLD.index_kind='semantic_text' AND CAST(s.id AS TEXT)=OLD.subject_key)"),
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
        ("recap","recap_payloads","NEW.kind||':'||NEW.key","json_object('date',NEW.day,'key',NEW.key,'part',NEW.kind,'revision',NEW.revision,'content_hex',hex(NEW.data))"),
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
        db(conn.execute_batch(&format!("DROP TRIGGER IF EXISTS agent_vector_{operation};CREATE TRIGGER agent_vector_{operation} AFTER {operation} ON derived_embeddings WHEN cp_agent_enabled()
            BEGIN INSERT INTO agent_enqueue(envelope) SELECT cp_agent_emit('vector',NEW.index_kind||':'||s.id,'put',json_object('kind',NEW.index_kind,'screenshot_id',s.id,'model_id',NEW.model_id,'model_revision',NEW.model_revision,'vector_hex',hex(NEW.vector_f32)))
            FROM screenshots s WHERE s.is_deleted=0 AND ((NEW.index_kind='clip_image' AND s.image_hash=NEW.subject_key) OR (NEW.index_kind='semantic_text' AND CAST(s.id AS TEXT)=NEW.subject_key));END;
            DROP TRIGGER IF EXISTS agent_day_{operation};CREATE TRIGGER agent_day_{operation} AFTER {operation} ON recap_days WHEN cp_agent_enabled()
            BEGIN INSERT INTO agent_enqueue(envelope) SELECT cp_agent_emit('calendar',NEW.day,'put',json_set(COALESCE((SELECT data FROM recap_calendars WHERE day=NEW.day),json_object('date',NEW.day,'rule','recorded_bounds','start_ms',NEW.start_ms,'end_ms',NEW.end_ms,'start_utc_offset_seconds',0,'end_utc_offset_seconds',0,'period_boundaries_ms',json_array())),'$.source_revision',NEW.revision));END;")))?;
    }
    Ok(())
}
