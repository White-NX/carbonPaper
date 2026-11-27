use super::*;
use crate::source::RawOcrKey;
use rusqlite::types::Value;

pub(super) const SCHEMA:&str="
CREATE TABLE IF NOT EXISTS block_access(id INTEGER PRIMARY KEY,screenshot_id INTEGER,revision INTEGER NOT NULL,slots BLOB,envelope BLOB);
CREATE INDEX IF NOT EXISTS block_snapshot ON block_access(screenshot_id);";

pub(super) fn put(
    conn: &Connection,
    key: &[u8],
    binding: &AgentViewBinding,
    grant: &OcrBlockAccess,
    revision: i64,
) -> Result<(), String> {
    if grant.block_id <= 0
        || grant.screenshot_id <= 0
        || grant.scope_tag.len() != 32
        || grant.slots.is_empty()
        || grant.slots.len() > ocr_block_codec::MAX_ROWS.div_ceil(8)
        || grant.slots.last() == Some(&0)
    {
        return Err("AGENT_INVALID_OCR_ACCESS".into());
    }
    let raw = source::open_key(
        key,
        binding,
        "ocr_block",
        &grant.block_id.to_string(),
        &grant.scope_tag,
        &grant.key_envelope,
    )?;
    ocr_key_wire::verify_scope(
        &raw,
        &binding.dataset_id,
        grant.screenshot_id,
        grant.block_id,
        &grant.scope_tag,
    )
    .map_err(|_| "AGENT_SOURCE_INTEGRITY_FAILED")?;
    let slots = stored_slots(&grant.slots);
    db(conn.execute("INSERT INTO block_access VALUES(?1,?2,?3,?4,?5)
        ON CONFLICT(id) DO UPDATE SET screenshot_id=excluded.screenshot_id,revision=excluded.revision,
        slots=excluded.slots,envelope=excluded.envelope WHERE block_access.revision<excluded.revision",
        params![grant.block_id,grant.screenshot_id,revision,slots,grant.key_envelope]))?;
    Ok(())
}

pub(super) fn delete(conn: &Connection, id: &str, revision: i64) -> Result<usize, String> {
    db(conn.execute("INSERT INTO block_access(id,revision) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET
        revision=excluded.revision,slots=NULL,envelope=NULL WHERE block_access.revision<excluded.revision",
        params![id.parse::<i64>().map_err(|_|"AGENT_INVALID_RECORD")?,revision]))
}

// A dense prefix needs one SQLite integer. Keep explicit bits for holes.
fn stored_slots(mask: &[u8]) -> Value {
    if let Some((&last, head)) = mask.split_last() {
        let tail = last.trailing_ones();
        if tail == last.count_ones() && head.iter().all(|byte| *byte == 0xff) {
            return Value::Integer((head.len() * 8 + tail as usize) as i64);
        }
    }
    Value::Blob(mask.to_vec())
}

fn permits(mask: &Value, slot: usize) -> bool {
    match mask {
        Value::Integer(n) => {
            (1..=ocr_block_codec::MAX_ROWS as i64).contains(n) && slot < *n as usize
        }
        Value::Blob(bytes) => {
            bytes.len() <= ocr_block_codec::MAX_ROWS.div_ceil(8)
                && bytes
                    .get(slot / 8)
                    .is_some_and(|byte| byte & (1 << (slot % 8)) != 0)
        }
        _ => false,
    }
}

struct OpenBlock {
    decoded: ocr_block_codec::Decoded,
    processed_at: Option<i64>,
    screenshot: i64,
    tag: Vec<u8>,
    slots: Value,
}

#[derive(Default)]
pub(super) struct BlockCache {
    blocks: HashMap<i64, OpenBlock>,
    bytes: usize,
}

impl BlockCache {
    pub(super) fn read(
        &mut self,
        repo: &AgentRepository,
        group: &RawOcrKey,
        screenshot: i64,
        slot: i64,
        row: i64,
    ) -> Result<(String, ocr_block_codec::Metadata, Option<i64>), String> {
        let slot = usize::try_from(slot).map_err(|_| "AGENT_INVALID_OCR_SLOT")?;
        if !self.blocks.contains_key(&group.id) {
            let mut lookup=db(repo.conn.prepare_cached("SELECT slots,envelope FROM block_access WHERE id=?1 AND screenshot_id=?2 AND envelope IS NOT NULL"))?;
            let grant: Option<(Value, Vec<u8>)> = db(lookup
                .query_row(params![group.id, screenshot], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional())?;
            let (slots, sealed) =
                grant.ok_or("AGENT_SYNC_INCOMPLETE: OCR block authorization is unavailable")?;
            if !permits(&slots, slot) {
                return Err("AGENT_SOURCE_CHANGED".into());
            }
            let key = source::open_key(
                &repo.key,
                &repo.binding,
                "ocr_block",
                &group.id.to_string(),
                &group.scope_tag,
                &sealed,
            )?;
            let dataset = &repo.binding.dataset_id;
            let integrity = |_| "AGENT_SOURCE_INTEGRITY_FAILED";
            ocr_key_wire::verify_scope(&key, dataset, screenshot, group.id, &group.scope_tag)
                .map_err(integrity)?;
            let (ciphertext, processed_at) = repo.source()?.ocr_block(group.id, screenshot)?;
            let aad = ocr_key_wire::block_aad(dataset, screenshot, group.id).map_err(integrity)?;
            let plain =
                Zeroizing::new(ocr_key_wire::open(&key, &aad, &ciphertext).map_err(integrity)?);
            let decoded = ocr_block_codec::Decoded::open(&plain)?;
            if self.bytes + decoded.size() > 4 * 1024 * 1024 {
                self.blocks.clear();
                self.bytes = 0;
            }
            self.bytes += decoded.size();
            self.blocks.insert(
                group.id,
                OpenBlock {
                    decoded,
                    processed_at,
                    screenshot,
                    tag: group.scope_tag.clone(),
                    slots,
                },
            );
        }
        let block = &self.blocks[&group.id];
        if block.screenshot != screenshot
            || block.tag != group.scope_tag
            || !permits(&block.slots, slot)
        {
            return Err("AGENT_SOURCE_CHANGED".into());
        }
        Ok((
            block.decoded.text(slot, row)?.to_owned(),
            block.decoded.metadata_for(slot, row)?,
            block.processed_at,
        ))
    }
}

impl AgentRepository {
    pub fn block_envelope(
        &self,
        id: i64,
        shot: i64,
        tag: &[u8],
    ) -> Result<Option<Vec<u8>>, String> {
        let sealed:Option<Vec<u8>>=db(self.conn.query_row("SELECT envelope FROM block_access WHERE id=?1 AND screenshot_id=?2 AND envelope IS NOT NULL",params![id,shot],|r|r.get(0)).optional())?;
        // The source's scope tag is authenticated by the envelope itself. A
        // reused ID with a new tag must obtain an independent authorization.
        Ok(sealed.filter(|v| {
            source::open_key(
                &self.key,
                &self.binding,
                "ocr_block",
                &id.to_string(),
                tag,
                v,
            )
            .is_ok()
        }))
    }
}
