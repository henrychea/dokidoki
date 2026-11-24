use crate::config::CommitScope;
use crate::discovery::DetectedRepo;
use crate::journal::{IncidentRecord, RunRecord};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};

const MIGRATIONS: &[&str] = &[
    "CREATE TABLE repos (
        id INTEGER PRIMARY KEY,
        path TEXT NOT NULL UNIQUE,
        root TEXT NOT NULL,
        git_common_dir TEXT,
        enabled INTEGER NOT NULL DEFAULT 0
    );",
    "ALTER TABLE repos ADD COLUMN agent TEXT;
     ALTER TABLE repos ADD COLUMN model TEXT;
     ALTER TABLE repos ADD COLUMN state TEXT;
     ALTER TABLE repos ADD COLUMN last_sync_at INTEGER;
     ALTER TABLE repos ADD COLUMN next_run_at INTEGER;
     ALTER TABLE repos ADD COLUMN fail_count INTEGER NOT NULL DEFAULT 0;
     ALTER TABLE repos ADD COLUMN quarantine_until INTEGER;
     ALTER TABLE repos ADD COLUMN last_error TEXT;
     CREATE TABLE runs (
        id INTEGER PRIMARY KEY,
        repo_id INTEGER NOT NULL,
        started_at INTEGER NOT NULL,
        finished_at INTEGER,
        state TEXT NOT NULL,
        head_before TEXT,
        head_after TEXT,
        agent TEXT,
        model TEXT,
        message TEXT,
        exit_code INTEGER,
        error_kind TEXT,
        pushed INTEGER NOT NULL DEFAULT 0,
        warning TEXT
     );
     CREATE INDEX runs_by_repo ON runs(repo_id, id);
     CREATE TABLE incidents (
        id INTEGER PRIMARY KEY,
        repo_id INTEGER NOT NULL,
        run_id INTEGER,
        severity TEXT NOT NULL,
        kind TEXT NOT NULL,
        detail TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        acknowledged_at INTEGER,
        resolved_at INTEGER
     );
     CREATE INDEX incidents_by_repo ON incidents(repo_id, id);
     CREATE TABLE snapshots (
        id INTEGER PRIMARY KEY,
        repo_id INTEGER NOT NULL,
        run_id INTEGER,
        ref TEXT NOT NULL,
        sha TEXT NOT NULL,
        created_at INTEGER NOT NULL
     );
     CREATE INDEX snapshots_by_run ON snapshots(run_id, id);
     CREATE TABLE message_cache (
        diff_hash TEXT NOT NULL,
        agent TEXT NOT NULL,
        model TEXT NOT NULL,
        message TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        PRIMARY KEY (diff_hash, agent, model)
     );",
    "ALTER TABLE repos ADD COLUMN allow_protected_push INTEGER NOT NULL DEFAULT 0;",
    // The center writes a per-repo scope override; it stays machine-side, not in .doki.toml.
    "ALTER TABLE repos ADD COLUMN commit_scope TEXT;",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoState {
    Clean,
    Dirty,
    Running,
    WaitingUser,
    Blocked,
    Quarantined,
    NoRemote,
    Disabled,
}

impl RepoState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Dirty => "dirty",
            Self::Running => "running",
            Self::WaitingUser => "waiting",
            Self::Blocked => "blocked",
            Self::Quarantined => "quarantined",
            Self::NoRemote => "no-remote",
            Self::Disabled => "disabled",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|state| state.label() == text)
    }

    const ALL: [Self; 8] = [
        Self::Clean,
        Self::Dirty,
        Self::Running,
        Self::WaitingUser,
        Self::Blocked,
        Self::Quarantined,
        Self::NoRemote,
        Self::Disabled,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRow {
    pub id: i64,
    pub path: PathBuf,
    pub root: String,
    pub git_common_dir: Option<PathBuf>,
    pub enabled: bool,
    pub state: Option<RepoState>,
    pub last_sync_at: Option<i64>,
    pub next_run_at: Option<i64>,
    pub fail_count: u32,
    pub quarantine_until: Option<i64>,
    pub last_error: Option<String>,
    pub allow_protected_push: bool,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub commit_scope: Option<CommitScope>,
}

impl RepoRow {
    pub fn quarantined(&self) -> bool {
        self.state == Some(RepoState::Quarantined)
    }
}

impl RepoRow {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    pub id: i64,
    pub repo_id: i64,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub state: String,
    pub head_before: Option<String>,
    pub head_after: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub message: Option<String>,
    pub exit_code: Option<i32>,
    pub error_kind: Option<String>,
    pub pushed: bool,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRow {
    pub id: i64,
    pub repo_id: i64,
    pub run_id: Option<i64>,
    pub reference: String,
    pub sha: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncidentRow {
    pub id: i64,
    pub repo_id: i64,
    pub run_id: Option<i64>,
    pub severity: String,
    pub kind: String,
    pub detail: String,
    pub created_at: i64,
}

#[derive(Debug, Default)]
pub struct RunFinish<'a> {
    pub run_id: i64,
    pub finished_at: i64,
    pub state: &'a str,
    pub head_after: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub model: Option<&'a str>,
    pub message: Option<&'a str>,
    pub exit_code: Option<i32>,
    pub error_kind: Option<&'a str>,
    pub warning: Option<&'a str>,
}

pub struct NewIncident<'a> {
    pub repo_id: i64,
    pub run_id: Option<i64>,
    pub severity: &'a str,
    pub kind: &'a str,
    pub detail: &'a str,
    pub created_at: i64,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        Self::migrate(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::migrate(Connection::open_in_memory()?)
    }

    fn migrate(conn: Connection) -> Result<Self> {
        let version: usize = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > MIGRATIONS.len() {
            bail!("database schema v{version} is newer than this doki");
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version) {
            conn.execute_batch(&format!(
                "BEGIN; {sql} PRAGMA user_version = {}; COMMIT;",
                i + 1
            ))?;
        }
        Ok(Self { conn })
    }

    /// New repos are inserted disabled; existing rows keep their `enabled` flag.
    pub fn upsert_detected(&mut self, repos: &[DetectedRepo]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO repos (path, root, git_common_dir) VALUES (?1, ?2, ?3)
                 ON CONFLICT(path) DO UPDATE SET root = excluded.root,
                     git_common_dir = excluded.git_common_dir",
            )?;
            for r in repos {
                let path = r.path.canonicalize().unwrap_or_else(|_| r.path.clone());
                stmt.execute(params![
                    path.to_string_lossy(),
                    r.root,
                    r.git_common_dir.as_ref().map(|p| p.to_string_lossy())
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn repos(&self) -> Result<Vec<RepoRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, root, git_common_dir, enabled, state, last_sync_at, next_run_at,
                 fail_count, quarantine_until, last_error, allow_protected_push,
                 agent, model, commit_scope
             FROM repos ORDER BY root, path",
        )?;
        let rows = stmt.query_map([], repo_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn repo(&self, id: i64) -> Result<Option<RepoRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, root, git_common_dir, enabled, state, last_sync_at, next_run_at,
                 fail_count, quarantine_until, last_error, allow_protected_push,
                 agent, model, commit_scope
             FROM repos WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map([id], repo_row)?;
        Ok(rows.next().transpose()?)
    }

    pub fn repo_by_path(&self, path: &Path) -> Result<Option<RepoRow>> {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut stmt = self.conn.prepare(
            "SELECT id, path, root, git_common_dir, enabled, state, last_sync_at, next_run_at,
                 fail_count, quarantine_until, last_error, allow_protected_push,
                 agent, model, commit_scope
             FROM repos WHERE path = ?1",
        )?;
        let mut rows = stmt.query_map([path.to_string_lossy()], repo_row)?;
        Ok(rows.next().transpose()?)
    }

    pub fn set_enabled(&self, id: i64, enabled: bool) -> Result<()> {
        let state = if enabled {
            None
        } else {
            Some(RepoState::Disabled)
        };
        let n = self.conn.execute(
            "UPDATE repos SET enabled = ?1, state = ?2 WHERE id = ?3",
            params![enabled, state.map(RepoState::label), id],
        )?;
        if n == 0 {
            bail!("no repo with id {id}");
        }
        Ok(())
    }

    /// A repo preference written by the center, so the studio can switch agents
    /// and scope without editing the user's config file.
    pub fn set_agent_pref(&self, id: i64, agent: &str, model: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE repos SET agent = ?1, model = ?2 WHERE id = ?3",
            params![agent, model, id],
        )?;
        if n == 0 {
            bail!("no repo with id {id}");
        }
        Ok(())
    }

    pub fn set_scope_pref(&self, id: i64, scope: Option<CommitScope>) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE repos SET commit_scope = ?1 WHERE id = ?2",
            params![scope.map(CommitScope::label), id],
        )?;
        if n == 0 {
            bail!("no repo with id {id}");
        }
        Ok(())
    }

    pub fn set_state(&self, id: i64, state: RepoState) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE repos SET state = ?1 WHERE id = ?2",
            params![state.label(), id],
        )?;
        if n == 0 {
            bail!("no repo with id {id}");
        }
        Ok(())
    }

    pub fn set_next_run(&self, id: i64, at: Option<i64>) -> Result<()> {
        self.conn.execute(
            "UPDATE repos SET next_run_at = ?1 WHERE id = ?2",
            params![at, id],
        )?;
        Ok(())
    }

    /// Only the machine side may grant this; `.doki.toml` can never enable a push.
    pub fn set_allow_protected_push(&self, id: i64, allow: bool) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE repos SET allow_protected_push = ?1 WHERE id = ?2",
            params![allow, id],
        )?;
        if n == 0 {
            bail!("no repo with id {id}");
        }
        Ok(())
    }

    pub fn record_success(
        &self,
        id: i64,
        at: i64,
        state: RepoState,
        next_run_at: Option<i64>,
    ) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE repos SET state = ?1, last_sync_at = ?2, next_run_at = ?3,
                 fail_count = 0, quarantine_until = NULL, last_error = NULL
             WHERE id = ?4",
            params![state.label(), at, next_run_at, id],
        )?;
        if n == 0 {
            bail!("no repo with id {id}");
        }
        Ok(())
    }

    /// A quarantined repo gets no next run, so a failing repo cannot retry forever.
    pub fn record_failure(
        &self,
        id: i64,
        at: i64,
        state: RepoState,
        error: &str,
        next_run_at: Option<i64>,
        quarantine_after: u32,
    ) -> Result<(u32, bool)> {
        let fail_count: u32 = self.conn.query_row(
            "UPDATE repos SET last_sync_at = ?1, last_error = ?2, fail_count = fail_count + 1,
                 next_run_at = ?3
             WHERE id = ?4 RETURNING fail_count",
            params![at, error, next_run_at, id],
            |r| r.get(0),
        )?;
        let quarantined = fail_count >= quarantine_after;
        self.conn.execute(
            "UPDATE repos SET state = ?1, next_run_at = ?2, quarantine_until = ?3 WHERE id = ?4",
            params![
                if quarantined {
                    RepoState::Quarantined.label()
                } else {
                    state.label()
                },
                if quarantined { None } else { next_run_at },
                quarantined.then_some(at),
                id
            ],
        )?;
        Ok((fail_count, quarantined))
    }

    pub fn reset_quarantine(&self, id: i64) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE repos SET fail_count = 0, quarantine_until = NULL, last_error = NULL,
                 next_run_at = NULL, state = ?1
             WHERE id = ?2",
            params![RepoState::Clean.label(), id],
        )?;
        if n == 0 {
            bail!("no repo with id {id}");
        }
        Ok(())
    }

    pub fn begin_run(
        &self,
        repo_id: i64,
        started_at: i64,
        head_before: Option<&str>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO runs (repo_id, started_at, state, head_before)
             VALUES (?1, ?2, 'running', ?3)",
            params![repo_id, started_at, head_before],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_run(&self, finish: RunFinish<'_>) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE runs SET finished_at = ?1, state = ?2, head_after = ?3, agent = ?4,
                 model = ?5, message = ?6, exit_code = ?7, error_kind = ?8, warning = ?9
             WHERE id = ?10",
            params![
                finish.finished_at,
                finish.state,
                finish.head_after,
                finish.agent,
                finish.model,
                finish.message,
                finish.exit_code,
                finish.error_kind,
                finish.warning,
                finish.run_id
            ],
        )?;
        if n == 0 {
            bail!("no run with id {}", finish.run_id);
        }
        Ok(())
    }

    pub fn mark_run_pushed(&self, run_id: i64, pushed: bool) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE runs SET pushed = ?1 WHERE id = ?2",
            params![pushed, run_id],
        )?;
        if n == 0 {
            bail!("no run with id {run_id}");
        }
        Ok(())
    }

    /// Only the push verdict changes, so the commit message stays in the journal.
    pub fn finish_push(
        &self,
        run_id: i64,
        finished_at: i64,
        state: &str,
        error_kind: &str,
        warning: Option<&str>,
    ) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE runs SET finished_at = ?1, state = ?2, error_kind = ?3, warning = ?4
             WHERE id = ?5",
            params![finished_at, state, error_kind, warning, run_id],
        )?;
        if n == 0 {
            bail!("no run with id {run_id}");
        }
        Ok(())
    }

    pub fn last_run(&self, repo_id: i64) -> Result<Option<RunRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, started_at, finished_at, state, head_before, head_after,
                 agent, model, message, exit_code, error_kind, pushed, warning
             FROM runs WHERE repo_id = ?1 ORDER BY id DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map([repo_id], run_row)?;
        Ok(rows.next().transpose()?)
    }

    pub fn runs(&self, repo_id: i64) -> Result<Vec<RunRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, started_at, finished_at, state, head_before, head_after,
                 agent, model, message, exit_code, error_kind, pushed, warning
             FROM runs WHERE repo_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([repo_id], run_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn add_snapshot(
        &self,
        repo_id: i64,
        run_id: i64,
        reference: &str,
        sha: &str,
        created_at: i64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO snapshots (repo_id, run_id, ref, sha, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![repo_id, run_id, reference, sha, created_at],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn last_snapshot(&self, run_id: i64) -> Result<Option<SnapshotRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, run_id, ref, sha, created_at
             FROM snapshots WHERE run_id = ?1 ORDER BY id DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map([run_id], |r| {
            Ok(SnapshotRow {
                id: r.get(0)?,
                repo_id: r.get(1)?,
                run_id: r.get(2)?,
                reference: r.get(3)?,
                sha: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn add_incident(&self, incident: NewIncident<'_>) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO incidents (repo_id, run_id, severity, kind, detail, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                incident.repo_id,
                incident.run_id,
                incident.severity,
                incident.kind,
                incident.detail,
                incident.created_at
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn incidents(&self, repo_id: i64) -> Result<Vec<IncidentRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_id, run_id, severity, kind, detail, created_at
             FROM incidents WHERE repo_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([repo_id], |r| {
            Ok(IncidentRow {
                id: r.get(0)?,
                repo_id: r.get(1)?,
                run_id: r.get(2)?,
                severity: r.get(3)?,
                kind: r.get(4)?,
                detail: r.get(5)?,
                created_at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Newest runs with their repo paths, the feed the journal is built from.
    pub fn journal_runs(&self, limit: usize) -> Result<Vec<RunRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT runs.id, runs.repo_id, repos.path, runs.started_at, runs.finished_at,
                 runs.state, runs.message, runs.agent, runs.model, runs.pushed,
                 runs.warning, runs.error_kind
             FROM runs JOIN repos ON repos.id = runs.repo_id
             ORDER BY runs.started_at DESC, runs.id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(RunRecord {
                id: r.get(0)?,
                repo_id: r.get(1)?,
                repo: PathBuf::from(r.get::<_, String>(2)?),
                started_at: r.get(3)?,
                finished_at: r.get(4)?,
                state: r.get(5)?,
                message: r.get(6)?,
                agent: r.get(7)?,
                model: r.get(8)?,
                pushed: r.get::<_, i64>(9)? != 0,
                warning: r.get(10)?,
                error_kind: r.get(11)?,
                upstream: None,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn journal_incidents(&self, limit: usize) -> Result<Vec<IncidentRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT incidents.id, incidents.repo_id, repos.path, incidents.run_id,
                 incidents.severity, incidents.kind, incidents.detail, incidents.created_at,
                 incidents.acknowledged_at
             FROM incidents JOIN repos ON repos.id = incidents.repo_id
             ORDER BY incidents.created_at DESC, incidents.id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(IncidentRecord {
                id: r.get(0)?,
                repo_id: r.get(1)?,
                repo: PathBuf::from(r.get::<_, String>(2)?),
                run_id: r.get(3)?,
                severity: r.get(4)?,
                kind: r.get(5)?,
                detail: r.get(6)?,
                created_at: r.get(7)?,
                acknowledged: r.get::<_, Option<i64>>(8)?.is_some(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn acknowledge_incident(&self, id: i64, at: i64) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE incidents SET acknowledged_at = ?1 WHERE id = ?2 AND acknowledged_at IS NULL",
            params![at, id],
        )?;
        if n == 0 {
            let exists: i64 =
                self.conn
                    .query_row("SELECT count(*) FROM incidents WHERE id = ?1", [id], |r| {
                        r.get(0)
                    })?;
            if exists == 0 {
                bail!("no incident with id {id}");
            }
        }
        Ok(())
    }

    pub fn cache_get(&self, diff_hash: &str, agent: &str, model: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT message FROM message_cache
                 WHERE diff_hash = ?1 AND agent = ?2 AND model = ?3",
                params![diff_hash, agent, model],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn cache_put(
        &self,
        diff_hash: &str,
        agent: &str,
        model: &str,
        message: &str,
        created_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO message_cache (diff_hash, agent, model, message, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(diff_hash, agent, model) DO UPDATE SET
                 message = excluded.message, created_at = excluded.created_at",
            params![diff_hash, agent, model, message, created_at],
        )?;
        Ok(())
    }
}

fn repo_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RepoRow> {
    Ok(RepoRow {
        id: r.get(0)?,
        path: PathBuf::from(r.get::<_, String>(1)?),
        root: r.get(2)?,
        git_common_dir: r.get::<_, Option<String>>(3)?.map(PathBuf::from),
        enabled: r.get(4)?,
        state: r
            .get::<_, Option<String>>(5)?
            .and_then(|text| RepoState::parse(&text)),
        last_sync_at: r.get(6)?,
        next_run_at: r.get(7)?,
        fail_count: r.get(8)?,
        quarantine_until: r.get(9)?,
        last_error: r.get(10)?,
        allow_protected_push: r.get::<_, i64>(11)? != 0,
        agent: r.get(12)?,
        model: r.get(13)?,
        commit_scope: r
            .get::<_, Option<String>>(14)?
            .and_then(|text| CommitScope::parse(&text)),
    })
}

fn run_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunRow> {
    Ok(RunRow {
        id: r.get(0)?,
        repo_id: r.get(1)?,
        started_at: r.get(2)?,
        finished_at: r.get(3)?,
        state: r.get(4)?,
        head_before: r.get(5)?,
        head_after: r.get(6)?,
        agent: r.get(7)?,
        model: r.get(8)?,
        message: r.get(9)?,
        exit_code: r.get(10)?,
        error_kind: r.get(11)?,
        pushed: r.get(12)?,
        warning: r.get(13)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(p: &str) -> DetectedRepo {
        DetectedRepo {
            root: "dev".into(),
            path: p.into(),
            git_common_dir: None,
        }
    }

    #[test]
    fn detected_repos_start_disabled_and_keep_flag_on_rescan() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_detected(&[det("/a"), det("/b")]).unwrap();
        assert!(s.repos().unwrap().iter().all(|r| !r.enabled));

        let a = s.repos().unwrap()[0].id;
        s.set_enabled(a, true).unwrap();
        s.upsert_detected(&[det("/a"), det("/b")]).unwrap();
        let rows = s.repos().unwrap();
        assert!(rows[0].enabled && !rows[1].enabled);
        assert!(s.set_enabled(999, true).is_err());
    }

    #[test]
    fn reopen_does_not_rerun_migrations() {
        let t = tempfile::tempdir().unwrap();
        let db = t.path().join("doki.db");
        Store::open(&db)
            .unwrap()
            .upsert_detected(&[det("/a")])
            .unwrap();
        assert_eq!(Store::open(&db).unwrap().repos().unwrap().len(), 1);
    }

    #[test]
    fn v1_database_migrates_without_losing_repos() {
        let t = tempfile::tempdir().unwrap();
        let db = t.path().join("doki.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(&format!(
                "{} INSERT INTO repos (path, root, git_common_dir, enabled)
                   VALUES ('/kept', 'dev', NULL, 1);
                 PRAGMA user_version = 1;",
                MIGRATIONS[0]
            ))
            .unwrap();
        }

        let store = Store::open(&db).unwrap();
        let repos = store.repos().unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].path, PathBuf::from("/kept"));
        assert!(repos[0].enabled);

        let conn = Connection::open(&db).unwrap();
        let version: usize = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 4);
        for table in ["runs", "incidents", "snapshots", "message_cache"] {
            let found: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(found, 1, "missing table {table}");
        }
        for column in [
            "state",
            "last_sync_at",
            "next_run_at",
            "fail_count",
            "quarantine_until",
            "last_error",
            "agent",
            "model",
            "allow_protected_push",
            "commit_scope",
        ] {
            let found: i64 = conn
                .query_row(
                    "SELECT count(*) FROM pragma_table_info('repos') WHERE name = ?1",
                    [column],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(found, 1, "missing repos column {column}");
        }
    }

    #[test]
    fn runs_snapshots_incidents_and_cache_round_trip() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_detected(&[det("/repo")]).unwrap();
        let repo = s.repos().unwrap()[0].id;

        let run = s.begin_run(repo, 100, Some("aaa")).unwrap();
        s.add_snapshot(repo, run, "refs/doki/pre-sync/100", "aaa", 100)
            .unwrap();
        s.finish_run(RunFinish {
            run_id: run,
            finished_at: 101,
            state: "committed",
            head_after: Some("bbb"),
            agent: Some("omp"),
            model: Some("m"),
            message: Some("doki: x"),
            exit_code: Some(0),
            error_kind: None,
            warning: Some("w"),
        })
        .unwrap();

        let last = s.last_run(repo).unwrap().unwrap();
        assert_eq!(last.head_before.as_deref(), Some("aaa"));
        assert_eq!(last.head_after.as_deref(), Some("bbb"));
        assert_eq!(last.state, "committed");
        assert!(!last.pushed);
        assert_eq!(last.warning.as_deref(), Some("w"));

        let snap = s.last_snapshot(run).unwrap().unwrap();
        assert_eq!(snap.sha, "aaa");
        assert_eq!(snap.reference, "refs/doki/pre-sync/100");

        s.mark_run_pushed(run, true).unwrap();
        assert!(s.last_run(repo).unwrap().unwrap().pushed);

        let incident = s
            .add_incident(NewIncident {
                repo_id: repo,
                run_id: Some(run),
                severity: "blocking",
                kind: "secret",
                detail: "found",
                created_at: 102,
            })
            .unwrap();
        assert!(incident > 0);
        assert_eq!(s.incidents(repo).unwrap()[0].kind, "secret");
        assert_eq!(s.runs(repo).unwrap().len(), 1);

        assert_eq!(s.cache_get("h", "omp", "m").unwrap(), None);
        s.cache_put("h", "omp", "m", "doki: cached", 1).unwrap();
        s.cache_put("h", "omp", "m", "doki: newer", 2).unwrap();
        assert_eq!(
            s.cache_get("h", "omp", "m").unwrap().as_deref(),
            Some("doki: newer")
        );
        assert_eq!(s.cache_get("h", "codex", "m").unwrap(), None);
    }

    #[test]
    fn journal_queries_join_repo_names_and_incidents_acknowledge_once() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_detected(&[det("/tmp/legacy-auth")]).unwrap();
        let repo = s.repos().unwrap()[0].id;
        let run = s.begin_run(repo, 100, None).unwrap();
        s.finish_run(RunFinish {
            run_id: run,
            finished_at: 101,
            state: "blocked",
            ..RunFinish::default()
        })
        .unwrap();
        let incident = s
            .add_incident(NewIncident {
                repo_id: repo,
                run_id: Some(run),
                severity: "blocking",
                kind: "diverged",
                detail: "Diverged upstream\nfatal: not a fast-forward",
                created_at: 102,
            })
            .unwrap();

        let runs = s.journal_runs(20).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].repo, std::path::PathBuf::from("/tmp/legacy-auth"));
        assert_eq!(runs[0].state, "blocked");
        let incidents = s.journal_incidents(20).unwrap();
        assert_eq!(
            incidents[0].repo,
            std::path::PathBuf::from("/tmp/legacy-auth")
        );
        assert_eq!(incidents[0].run_id, Some(run));
        assert!(!incidents[0].acknowledged);

        s.acknowledge_incident(incident, 200).unwrap();
        assert!(s.journal_incidents(20).unwrap()[0].acknowledged);
        s.acknowledge_incident(incident, 300).unwrap();
        assert!(s.acknowledge_incident(999, 300).is_err());
    }

    #[test]
    fn repo_lookup_by_path_is_canonical() {
        let t = tempfile::tempdir().unwrap();
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_detected(&[DetectedRepo {
            root: "dev".into(),
            path: t.path().to_path_buf(),
            git_common_dir: None,
        }])
        .unwrap();
        let found = s.repo_by_path(t.path()).unwrap().unwrap();
        assert_eq!(found.id, s.repos().unwrap()[0].id);
        assert!(s.repo_by_path(&t.path().join("nope")).unwrap().is_none());
    }
}
