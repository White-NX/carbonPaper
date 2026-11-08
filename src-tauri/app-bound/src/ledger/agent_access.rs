//! Independent, service-owned authority for the text-only MCP reader.
use super::*;

const READ_TOOLS: &[&str] = &[
    "get_snapshots_by_time_range",
    "get_snapshot_details",
    "search_ocr_text",
    "search_nl",
    "get_smart_clusters",
    "get_smart_cluster_ocr_corpus",
    "get_smart_cluster_summary",
    "get_recap_days",
    "get_recap_day",
    "search_recaps",
];

struct View {
    binding: AgentViewBinding,
    enabled: bool,
    phase: String,
    revision: i64,
    source: i64,
    applied: i64,
    content: Option<Vec<u8>>,
    outbox: Option<Vec<u8>>,
}

impl Ledger {
    pub(super) fn init_agent_access(conn: &Connection) -> Result<()> {
        db(conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agent_views (
            owner TEXT PRIMARY KEY REFERENCES owners(sid), binding TEXT NOT NULL,
            enabled INTEGER NOT NULL, phase TEXT NOT NULL, revision INTEGER NOT NULL,
            source_revision INTEGER NOT NULL, applied_revision INTEGER NOT NULL,
            content_key BLOB, outbox_key BLOB);
            CREATE TABLE IF NOT EXISTS agent_grants (
            owner TEXT NOT NULL REFERENCES owners(sid), id TEXT PRIMARY KEY,
            generation TEXT NOT NULL, label TEXT NOT NULL, token_hash TEXT NOT NULL UNIQUE,
            created INTEGER NOT NULL, expires INTEGER, last_used INTEGER,
            revoked INTEGER NOT NULL DEFAULT 0, revision INTEGER NOT NULL DEFAULT 1);
            CREATE INDEX IF NOT EXISTS agent_grants_owner ON agent_grants(owner,generation);
            -- A restarted service needs desktop reconciliation before serving reads.
            UPDATE agent_views SET phase='syncing',source_revision=source_revision+1
                WHERE enabled=1;",
        ))
    }

    fn agent_view(conn: &Connection, sid: &str) -> Result<Option<View>> {
        let row: Option<(String,bool,String,i64,i64,i64,Option<Vec<u8>>,Option<Vec<u8>>)> = db(conn.query_row(
            "SELECT binding,enabled,phase,revision,source_revision,applied_revision,content_key,outbox_key
                FROM agent_views WHERE owner=?1", [sid],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional())?;
        row.map(
            |(binding, enabled, phase, revision, source, applied, content, outbox)| {
                Ok(View {
                    binding: serde_json::from_str(&binding).map_err(|_| BrokerError::Integrity)?,
                    enabled,
                    phase,
                    revision,
                    source,
                    applied,
                    content,
                    outbox,
                })
            },
        )
        .transpose()
    }

    fn agent_grants(conn: &Connection, sid: &str) -> Result<Vec<AgentGrant>> {
        let mut stmt = db(conn.prepare(
            "SELECT id,label,created,expires,last_used,revoked
            FROM agent_grants WHERE owner=?1 ORDER BY created DESC,id",
        ))?;
        let rows = db(stmt.query_map([sid], |r| {
            Ok(AgentGrant {
                id: r.get(0)?,
                label: r.get(1)?,
                created_at_ms: r.get(2)?,
                expires_at_ms: r.get(3)?,
                last_used_at_ms: r.get(4)?,
                revoked: r.get(5)?,
                capabilities: READ_TOOLS.iter().map(|v| (*v).into()).collect(),
            })
        }))?;
        db(rows.collect())
    }

    fn agent_status(conn: &Connection, who: &Principal) -> Result<AgentAccessStatus> {
        let view = Self::agent_view(conn, &who.sid)?;
        Ok(AgentAccessStatus {
            enabled: view.as_ref().is_some_and(|v| v.enabled),
            binding: view.as_ref().map(|v| v.binding.clone()),
            phase: view.as_ref().map_or("disabled".into(), |v| v.phase.clone()),
            source_revision: view.as_ref().map_or(0, |v| v.source),
            applied_revision: view.as_ref().map_or(0, |v| v.applied),
            grants: Self::agent_grants(conn, &who.sid)?,
        })
    }

    pub(super) fn retire_agent_access(conn: &Connection, sid: &str) -> Result<()> {
        db(conn.execute(
            "UPDATE agent_grants SET revoked=1,revision=revision+1 WHERE owner=?1 AND revoked=0",
            [sid],
        ))?;
        db(conn.execute(
            "UPDATE agent_views SET enabled=0,phase='disabled',revision=revision+1,
            content_key=NULL,outbox_key=NULL WHERE owner=?1",
            [sid],
        ))?;
        Ok(())
    }

    pub(super) fn dispatch_agent_access(
        conn: &Connection,
        who: &Principal,
        request: &Request,
        now: i64,
        protector: &dyn KeyProtector,
    ) -> Result<Option<Response>> {
        let is_agent = matches!(
            request,
            Request::AgentStatus {}
                | Request::AgentPrepare { .. }
                | Request::AgentWriterKeys { .. }
                | Request::AgentReaderKeys { .. }
                | Request::AgentFence { .. }
                | Request::AgentPublish { .. }
                | Request::AgentIssue { .. }
                | Request::AgentRevoke { .. }
                | Request::AgentDisable {}
                | Request::AgentAuthorize { .. }
                | Request::AgentCheck { .. }
        );
        if !is_agent {
            return Ok(None);
        }
        if who.role == RuntimeRole::McpReader
            && !matches!(
                request,
                Request::AgentReaderKeys { .. }
                    | Request::AgentAuthorize { .. }
                    | Request::AgentCheck { .. }
            )
        {
            return Err(BrokerError::AccessDenied);
        }
        let now_ms = now.saturating_mul(1000);
        match request {
            Request::AgentStatus {} => {
                return Ok(Some(Response::AgentStatus(Self::agent_status(conn, who)?)))
            }
            Request::AgentPrepare {
                dataset_id,
                view_path,
            } => {
                if !valid_id(dataset_id)
                    || view_path.len() > 32700
                    || view_path.contains('\0')
                    || !std::path::Path::new(view_path).is_absolute()
                    || std::path::Path::new(view_path)
                        .file_name()
                        .is_none_or(|v| v != "agent-memory.db")
                    || view_path.starts_with(r"\\") && !view_path.starts_with(r"\\?\")
                {
                    return Err(BrokerError::InvalidRequest);
                }
                if Self::agent_view(conn, &who.sid)?.is_some_and(|v| v.enabled) {
                    return Err(BrokerError::Busy);
                }
                let binding = AgentViewBinding {
                    dataset_id: dataset_id.clone(),
                    generation: random_id(),
                    view_path: view_path.clone(),
                };
                let content = new_key();
                let outbox = new_key();
                let content_blob = protector.protect(&content)?;
                let outbox_blob = protector.protect(&outbox)?;
                let json = serde_json::to_string(&binding).map_err(|_| BrokerError::Integrity)?;
                db(conn.execute("INSERT INTO agent_views VALUES(?1,?2,1,'preparing',1,1,0,?3,?4)
                    ON CONFLICT(owner) DO UPDATE SET binding=excluded.binding,enabled=1,phase='preparing',
                    revision=agent_views.revision+1,source_revision=agent_views.source_revision+1,
                    applied_revision=0,content_key=excluded.content_key,outbox_key=excluded.outbox_key",
                    params![who.sid,json,content_blob,outbox_blob]))?;
                return Ok(Some(Response::AgentKeys(AgentViewKeys {
                    binding,
                    content_key: content,
                    outbox_key: Some(outbox),
                })));
            }
            Request::AgentDisable {} => {
                Self::retire_agent_access(conn, &who.sid)?;
                return Ok(Some(Response::Ok));
            }
            Request::AgentRevoke { grant_id } => {
                if !valid_id(grant_id) {
                    return Err(BrokerError::InvalidRequest);
                }
                db(conn.execute(
                    "UPDATE agent_grants SET revoked=1,revision=revision+1
                    WHERE owner=?1 AND id=?2 AND revoked=0",
                    params![who.sid, grant_id],
                ))?;
                return Ok(Some(Response::Ok));
            }
            _ => {}
        }
        let view = Self::agent_view(conn, &who.sid)?.ok_or(BrokerError::Disabled)?;
        if !view.enabled {
            return Err(BrokerError::Disabled);
        }
        let generation = match request {
            Request::AgentWriterKeys { generation }
            | Request::AgentReaderKeys { generation }
            | Request::AgentFence { generation }
            | Request::AgentPublish { generation, .. }
            | Request::AgentIssue { generation, .. } => Some(generation.as_str()),
            Request::AgentCheck { context } => Some(context.generation.as_str()),
            _ => None,
        };
        if generation.is_some_and(|g| g != view.binding.generation) {
            return Err(BrokerError::DatasetMismatch);
        }
        match request {
            Request::AgentWriterKeys { .. } | Request::AgentReaderKeys { .. } => {
                if who.role == RuntimeRole::McpReader && view.phase != "ready" {
                    return Err(BrokerError::Busy);
                }
                let content_key =
                    protector.unprotect(view.content.as_deref().ok_or(BrokerError::Retired)?)?;
                let outbox_key = if who.role == RuntimeRole::Desktop {
                    Some(protector.unprotect(view.outbox.as_deref().ok_or(BrokerError::Retired)?)?)
                } else {
                    None
                };
                Ok(Some(Response::AgentKeys(AgentViewKeys {
                    binding: view.binding,
                    content_key,
                    outbox_key,
                })))
            }
            Request::AgentFence { .. } => {
                let source = view
                    .source
                    .checked_add(1)
                    .ok_or(BrokerError::LimitExceeded)?;
                db(conn.execute(
                    "UPDATE agent_views SET source_revision=?2,phase='syncing' WHERE owner=?1",
                    params![who.sid, source],
                ))?;
                Ok(Some(Response::AgentRevision(source)))
            }
            Request::AgentPublish {
                revision, complete, ..
            } => {
                if *revision != view.source || *revision < view.applied {
                    return Err(BrokerError::Integrity);
                }
                db(conn.execute(
                    "UPDATE agent_views SET applied_revision=?2,phase=?3 WHERE owner=?1",
                    params![who.sid, revision, if *complete { "ready" } else { "gap" }],
                ))?;
                Ok(Some(Response::Ok))
            }
            Request::AgentIssue {
                label,
                token_hash,
                expires_at_ms,
                ..
            } => {
                if view.phase != "ready" || view.source != view.applied {
                    return Err(BrokerError::Busy);
                }
                if label.trim().is_empty()
                    || label.chars().count() > 80
                    || label.chars().any(char::is_control)
                    || !valid_id(token_hash)
                    || expires_at_ms.is_some_and(|v| v <= now_ms)
                {
                    return Err(BrokerError::InvalidRequest);
                }
                let count: i64 = db(conn.query_row(
                    "SELECT COUNT(*) FROM agent_grants WHERE owner=?1 AND revoked=0",
                    [&who.sid],
                    |r| r.get(0),
                ))?;
                if count >= 64 {
                    return Err(BrokerError::LimitExceeded);
                }
                let id = random_id();
                db(conn.execute(
                    "INSERT INTO agent_grants(owner,id,generation,label,token_hash,created,expires)
                    VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        who.sid,
                        id,
                        view.binding.generation,
                        label.trim(),
                        token_hash,
                        now_ms,
                        expires_at_ms
                    ],
                ))?;
                let grant = Self::agent_grants(conn, &who.sid)?
                    .into_iter()
                    .find(|g| g.id == id)
                    .ok_or(BrokerError::Storage)?;
                Ok(Some(Response::AgentGrant(grant)))
            }
            Request::AgentAuthorize { token_hash } => {
                if !valid_id(token_hash) {
                    return Err(BrokerError::AccessDenied);
                }
                let row:Option<(String,i64)> = db(conn.query_row("SELECT id,revision FROM agent_grants
                    WHERE owner=?1 AND generation=?2 AND token_hash=?3 AND revoked=0 AND (expires IS NULL OR expires>?4)",
                    params![who.sid,view.binding.generation,token_hash,now_ms],|r|Ok((r.get(0)?,r.get(1)?))).optional())?;
                let (id, grant_revision) = row.ok_or(BrokerError::AccessDenied)?;
                if view.phase != "ready" || view.source != view.applied {
                    return Err(BrokerError::Busy);
                }
                db(conn.execute(
                    "UPDATE agent_grants SET last_used=?2 WHERE id=?1",
                    params![id, now_ms],
                ))?;
                Ok(Some(Response::AgentContext(AgentRequestContext {
                    grant_id: id,
                    owner: who.sid.clone(),
                    dataset_id: view.binding.dataset_id,
                    generation: view.binding.generation,
                    authorization_revision: view.revision,
                    grant_revision,
                    source_revision: view.source,
                })))
            }
            Request::AgentCheck { context } => {
                if context.owner != who.sid
                    || context.dataset_id != view.binding.dataset_id
                    || context.authorization_revision != view.revision
                    || context.source_revision != view.source
                    || view.phase != "ready"
                    || view.source != view.applied
                {
                    return Err(BrokerError::AccessDenied);
                }
                let valid:bool = db(conn.query_row("SELECT EXISTS(SELECT 1 FROM agent_grants WHERE owner=?1 AND id=?2
                    AND generation=?3 AND revision=?4 AND revoked=0 AND (expires IS NULL OR expires>?5))",
                    params![who.sid,context.grant_id,context.generation,context.grant_revision,now_ms],|r|r.get(0)))?;
                if !valid {
                    return Err(BrokerError::AccessDenied);
                }
                Ok(Some(Response::Ok))
            }
            _ => Err(BrokerError::InvalidRequest),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{fixture, TestProtection};
    use super::*;
    fn call(ledger: &mut Ledger, who: &Principal, request: Request) -> Result<Response> {
        ledger.handle(who, request, 110, &TestProtection)
    }
    fn ready(ledger: &mut Ledger, who: &Principal, dataset: &str) -> AgentViewKeys {
        let path = std::env::temp_dir()
            .join("agent-memory.db")
            .to_string_lossy()
            .into_owned();
        let Response::AgentKeys(keys) = call(
            ledger,
            who,
            Request::AgentPrepare {
                dataset_id: dataset.into(),
                view_path: path,
            },
        )
        .unwrap() else {
            panic!()
        };
        let Response::AgentStatus(status) = call(ledger, who, Request::AgentStatus {}).unwrap()
        else {
            panic!()
        };
        call(
            ledger,
            who,
            Request::AgentPublish {
                generation: keys.binding.generation.clone(),
                revision: status.source_revision,
                complete: true,
            },
        )
        .unwrap();
        keys
    }
    #[test]
    fn read_grants_fence_source_changes_and_independent_revocation() {
        let (mut ledger, who, dataset) = fixture();
        let keys = ready(&mut ledger, &who, &dataset);
        let issue = |label: &str, hash: &str| Request::AgentIssue {
            generation: keys.binding.generation.clone(),
            label: label.into(),
            token_hash: hash.into(),
            expires_at_ms: None,
        };
        let Response::AgentGrant(first) =
            call(&mut ledger, &who, issue("one", &"a".repeat(64))).unwrap()
        else {
            panic!()
        };
        call(&mut ledger, &who, issue("two", &"b".repeat(64))).unwrap();
        let mut reader = who.clone();
        reader.role = RuntimeRole::McpReader;
        let Response::AgentContext(context) = call(
            &mut ledger,
            &reader,
            Request::AgentAuthorize {
                token_hash: "a".repeat(64),
            },
        )
        .unwrap() else {
            panic!()
        };
        call(
            &mut ledger,
            &who,
            Request::AgentRevoke { grant_id: first.id },
        )
        .unwrap();
        assert!(call(&mut ledger, &reader, Request::AgentCheck { context }).is_err());
        let Response::AgentContext(other) = call(
            &mut ledger,
            &reader,
            Request::AgentAuthorize {
                token_hash: "b".repeat(64),
            },
        )
        .unwrap() else {
            panic!()
        };
        assert!(call(
            &mut ledger,
            &reader,
            Request::AgentAuthorize {
                token_hash: "c".repeat(64)
            }
        )
        .is_err());
        let Response::AgentRevision(revision) = call(
            &mut ledger,
            &who,
            Request::AgentFence {
                generation: keys.binding.generation.clone(),
            },
        )
        .unwrap() else {
            panic!()
        };
        assert!(call(&mut ledger, &reader, Request::AgentCheck { context: other }).is_err());
        call(
            &mut ledger,
            &who,
            Request::AgentPublish {
                generation: keys.binding.generation.clone(),
                revision,
                complete: true,
            },
        )
        .unwrap();
        // Organization policy does not revoke Agent keys or credentials.
        call(
            &mut ledger,
            &who,
            Request::SetPolicy {
                enabled: false,
                limits: Limits::default(),
            },
        )
        .unwrap();
        assert!(call(
            &mut ledger,
            &reader,
            Request::AgentAuthorize {
                token_hash: "b".repeat(64)
            }
        )
        .is_ok());
        call(&mut ledger, &who, Request::AgentDisable {}).unwrap();
        assert!(call(
            &mut ledger,
            &reader,
            Request::AgentReaderKeys {
                generation: keys.binding.generation
            }
        )
        .is_err());
    }
    #[test]
    fn reader_role_cannot_issue_grants_or_acquire_any_archive_or_processing_key() {
        let (mut ledger, who, dataset) = fixture();
        let keys = ready(&mut ledger, &who, &dataset);
        let mut reader = who.clone();
        reader.role = RuntimeRole::McpReader;
        for request in [
            Request::Status {},
            Request::AgentWriterKeys {
                generation: keys.binding.generation.clone(),
            },
            Request::AgentDisable {},
            Request::AgentIssue {
                generation: keys.binding.generation.clone(),
                label: "x".into(),
                token_hash: "a".repeat(64),
                expires_at_ms: None,
            },
            Request::AttachDataset {
                dataset_id: dataset,
            },
            Request::RevokeRecap {},
        ] {
            assert_eq!(
                call(&mut ledger, &reader, request).unwrap_err(),
                BrokerError::AccessDenied
            );
        }
        let Response::AgentKeys(read_keys) = call(
            &mut ledger,
            &reader,
            Request::AgentReaderKeys {
                generation: keys.binding.generation,
            },
        )
        .unwrap() else {
            panic!()
        };
        assert!(read_keys.outbox_key.is_none());
        assert_eq!(read_keys.content_key.0, keys.content_key.0);
        assert_ne!(keys.content_key.0, keys.outbox_key.unwrap().0);
    }
    #[test]
    fn expiry_and_dataset_generation_fail_closed() {
        let (mut ledger, who, dataset) = fixture();
        let keys = ready(&mut ledger, &who, &dataset);
        call(
            &mut ledger,
            &who,
            Request::AgentIssue {
                generation: keys.binding.generation.clone(),
                label: "expiring".into(),
                token_hash: "a".repeat(64),
                expires_at_ms: Some(111000),
            },
        )
        .unwrap();
        assert!(ledger
            .handle(
                &who,
                Request::AgentAuthorize {
                    token_hash: "a".repeat(64)
                },
                111,
                &TestProtection
            )
            .is_err());
        call(
            &mut ledger,
            &who,
            Request::RetireDataset {
                dataset_id: keys.binding.dataset_id.clone(),
            },
        )
        .unwrap();
        assert!(call(
            &mut ledger,
            &who,
            Request::AgentWriterKeys {
                generation: keys.binding.generation
            }
        )
        .is_err());
    }
}
