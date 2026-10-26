//! SQLite persistence. One file at `data/localgallery.db`.

use anyhow::{Context, Result};
use lg_types::{Job, JobSpec, JobStatus, Output, RunMetrics, now_ms};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY,
    spec TEXT NOT NULL,
    status TEXT NOT NULL,
    position INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    finished_at INTEGER,
    error TEXT,
    outputs TEXT NOT NULL DEFAULT '[]',
    metrics TEXT
);
CREATE INDEX IF NOT EXISTS jobs_status ON jobs(status, position);
CREATE TABLE IF NOT EXISTS outputs (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL,
    path TEXT NOT NULL,
    thumb TEXT,
    media TEXT NOT NULL,
    width INTEGER,
    height INTEGER,
    model TEXT NOT NULL,
    op TEXT NOT NULL,
    prompt TEXT NOT NULL DEFAULT '',
    seed INTEGER,
    created_at INTEGER NOT NULL,
    favorite INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS outputs_created ON outputs(created_at DESC);
CREATE TABLE IF NOT EXISTS benchmarks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    model TEXT NOT NULL,
    op TEXT NOT NULL,
    precision TEXT,
    plan TEXT NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    frames INTEGER NOT NULL DEFAULT 1,
    steps INTEGER NOT NULL DEFAULT 0,
    seconds REAL NOT NULL,
    peak_vram_gb REAL,
    peak_ram_gb REAL,
    shared_gpu_gb REAL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS bench_model ON benchmarks(model, op);
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS assets (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    name TEXT NOT NULL,
    mime TEXT NOT NULL,
    width INTEGER,
    height INTEGER,
    created_at INTEGER NOT NULL
);
"#;

pub struct Store {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Benchmark {
    pub model: String,
    pub op: String,
    pub precision: Option<String>,
    pub plan: String,
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    pub steps: u32,
    pub seconds: f64,
    pub peak_vram_gb: Option<f64>,
    pub peak_ram_gb: Option<f64>,
    pub shared_gpu_gb: Option<f64>,
    #[serde(default)]
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Asset {
    pub id: String,
    /// Relative to the data dir.
    pub path: String,
    pub name: String,
    pub mime: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OutputQuery {
    pub search: Option<String>,
    pub model: Option<String>,
    pub media: Option<String>,
    pub favorites: Option<bool>,
    pub limit: Option<u32>,
    pub before: Option<i64>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::init(conn)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    // ---- jobs ----

    pub fn insert_job(&self, id: &str, spec: &JobSpec) -> Result<Job> {
        let c = self.conn.lock();
        let pos: i64 = c.query_row("SELECT COALESCE(MAX(position), 0) + 1 FROM jobs", [], |r| {
            r.get(0)
        })?;
        let now = now_ms();
        c.execute(
            "INSERT INTO jobs (id, spec, status, position, created_at) VALUES (?1, ?2, 'queued', ?3, ?4)",
            params![id, serde_json::to_string(spec)?, pos, now],
        )?;
        drop(c);
        self.job(id)?.context("job vanished after insert")
    }

    pub fn job(&self, id: &str) -> Result<Option<Job>> {
        let c = self.conn.lock();
        Ok(
            c.query_row("SELECT * FROM jobs WHERE id = ?1", [id], job_from_row)
                .optional()?,
        )
    }

    /// Most recent first, plus every unfinished job.
    pub fn jobs(&self, limit: u32) -> Result<Vec<Job>> {
        let c = self.conn.lock();
        let mut st = c.prepare(
            "SELECT * FROM jobs WHERE status IN ('queued','running')
             UNION ALL
             SELECT * FROM (SELECT * FROM jobs WHERE status NOT IN ('queued','running') ORDER BY finished_at DESC LIMIT ?1)
             ORDER BY position",
        )?;
        let rows = st.query_map([limit], job_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn queued(&self) -> Result<Vec<Job>> {
        let c = self.conn.lock();
        let mut st = c.prepare("SELECT * FROM jobs WHERE status = 'queued' ORDER BY position")?;
        let rows = st.query_map([], job_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn set_status(&self, id: &str, status: JobStatus, error: Option<&str>) -> Result<()> {
        let c = self.conn.lock();
        let now = now_ms();
        match status {
            JobStatus::Running => c.execute(
                "UPDATE jobs SET status = ?2, started_at = ?3, error = NULL WHERE id = ?1",
                params![id, status.as_str(), now],
            )?,
            s if s.is_final() => c.execute(
                "UPDATE jobs SET status = ?2, finished_at = ?3, error = ?4 WHERE id = ?1",
                params![id, status.as_str(), now, error],
            )?,
            _ => c.execute(
                "UPDATE jobs SET status = ?2, started_at = NULL, finished_at = NULL, error = NULL WHERE id = ?1",
                params![id, status.as_str()],
            )?,
        };
        Ok(())
    }

    pub fn finish_job(&self, id: &str, outputs: &[String], metrics: &RunMetrics) -> Result<()> {
        let c = self.conn.lock();
        c.execute(
            "UPDATE jobs SET status = 'done', finished_at = ?2, outputs = ?3, metrics = ?4, error = NULL WHERE id = ?1",
            params![id, now_ms(), serde_json::to_string(outputs)?, serde_json::to_string(metrics)?],
        )?;
        Ok(())
    }

    /// Put queued jobs in the given order; unlisted queued jobs keep their relative order after them.
    pub fn reorder(&self, ids: &[String]) -> Result<()> {
        let queued = self.queued()?;
        let mut order: Vec<String> = ids
            .iter()
            .filter(|id| queued.iter().any(|j| &j.id == *id))
            .cloned()
            .collect();
        for j in &queued {
            if !order.contains(&j.id) {
                order.push(j.id.clone());
            }
        }
        let base = queued.iter().map(|j| j.position).min().unwrap_or(1);
        let mut c = self.conn.lock();
        let tx = c.transaction()?;
        for (i, id) in order.iter().enumerate() {
            tx.execute(
                "UPDATE jobs SET position = ?2 WHERE id = ?1",
                params![id, base + i as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Jobs left running by a crash go back to the queue.
    pub fn recover(&self) -> Result<usize> {
        let c = self.conn.lock();
        Ok(c.execute(
            "UPDATE jobs SET status = 'queued', started_at = NULL WHERE status = 'running'",
            [],
        )?)
    }

    pub fn delete_job(&self, id: &str) -> Result<()> {
        self.conn.lock().execute(
            "DELETE FROM jobs WHERE id = ?1 AND status NOT IN ('running')",
            [id],
        )?;
        Ok(())
    }

    // ---- outputs ----

    pub fn insert_output(&self, o: &Output) -> Result<()> {
        self.conn.lock().execute(
            "INSERT INTO outputs (id, job_id, path, thumb, media, width, height, model, op, prompt, seed, created_at, favorite)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![o.id, o.job_id, o.path, o.thumb, o.media, o.width, o.height, o.model, o.op, o.prompt, o.seed, o.created_at, o.favorite],
        )?;
        Ok(())
    }

    pub fn output(&self, id: &str) -> Result<Option<Output>> {
        let c = self.conn.lock();
        Ok(
            c.query_row("SELECT * FROM outputs WHERE id = ?1", [id], output_from_row)
                .optional()?,
        )
    }

    pub fn outputs(&self, q: &OutputQuery) -> Result<Vec<Output>> {
        let mut sql = String::from("SELECT * FROM outputs WHERE 1=1");
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(s) = q.search.as_deref().filter(|s| !s.trim().is_empty()) {
            sql.push_str(" AND (prompt LIKE ? ESCAPE '\\' OR model LIKE ? ESCAPE '\\')");
            let pat = format!(
                "%{}%",
                s.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            );
            args.push(pat.clone().into());
            args.push(pat.into());
        }
        if let Some(m) = &q.model {
            sql.push_str(" AND model = ?");
            args.push(m.clone().into());
        }
        if let Some(m) = &q.media {
            sql.push_str(" AND media = ?");
            args.push(m.clone().into());
        }
        if q.favorites == Some(true) {
            sql.push_str(" AND favorite = 1");
        }
        if let Some(b) = q.before {
            sql.push_str(" AND created_at < ?");
            args.push(b.into());
        }
        sql.push_str(" ORDER BY created_at DESC LIMIT ?");
        args.push(i64::from(q.limit.unwrap_or(200).min(1000)).into());
        let c = self.conn.lock();
        let mut st = c.prepare(&sql)?;
        let rows = st.query_map(rusqlite::params_from_iter(args), output_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn set_favorite(&self, id: &str, fav: bool) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE outputs SET favorite = ?2 WHERE id = ?1",
            params![id, fav],
        )?;
        Ok(())
    }

    pub fn delete_output(&self, id: &str) -> Result<Option<Output>> {
        let o = self.output(id)?;
        self.conn
            .lock()
            .execute("DELETE FROM outputs WHERE id = ?1", [id])?;
        Ok(o)
    }

    // ---- benchmarks ----

    pub fn add_benchmark(&self, b: &Benchmark) -> Result<()> {
        self.conn.lock().execute(
            "INSERT INTO benchmarks (model, op, precision, plan, width, height, frames, steps, seconds, peak_vram_gb, peak_ram_gb, shared_gpu_gb, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![b.model, b.op, b.precision, b.plan, b.width, b.height, b.frames, b.steps, b.seconds, b.peak_vram_gb, b.peak_ram_gb, b.shared_gpu_gb, now_ms()],
        )?;
        Ok(())
    }

    pub fn benchmarks(&self, model: Option<&str>, limit: u32) -> Result<Vec<Benchmark>> {
        let c = self.conn.lock();
        let mut st = c.prepare(
            "SELECT model, op, precision, plan, width, height, frames, steps, seconds, peak_vram_gb, peak_ram_gb, shared_gpu_gb, created_at
             FROM benchmarks WHERE (?1 IS NULL OR model = ?1) ORDER BY created_at DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![model, limit], |r| {
            Ok(Benchmark {
                model: r.get(0)?,
                op: r.get(1)?,
                precision: r.get(2)?,
                plan: r.get(3)?,
                width: r.get(4)?,
                height: r.get(5)?,
                frames: r.get(6)?,
                steps: r.get(7)?,
                seconds: r.get(8)?,
                peak_vram_gb: r.get(9)?,
                peak_ram_gb: r.get(10)?,
                shared_gpu_gb: r.get(11)?,
                created_at: r.get(12)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    // ---- settings ----

    pub fn setting(&self, key: &str) -> Result<Option<Value>> {
        let c = self.conn.lock();
        let s: Option<String> = c
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(s.and_then(|s| serde_json::from_str(&s).ok()))
    }

    pub fn set_setting(&self, key: &str, v: &Value) -> Result<()> {
        self.conn.lock().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, serde_json::to_string(v)?],
        )?;
        Ok(())
    }

    pub fn settings(&self) -> Result<serde_json::Map<String, Value>> {
        let c = self.conn.lock();
        let mut st = c.prepare("SELECT key, value FROM settings")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = serde_json::Map::new();
        for row in rows {
            let (k, v) = row?;
            out.insert(k, serde_json::from_str(&v).unwrap_or(Value::Null));
        }
        Ok(out)
    }

    // ---- assets ----

    pub fn insert_asset(&self, a: &Asset) -> Result<()> {
        self.conn.lock().execute(
            "INSERT INTO assets (id, path, name, mime, width, height, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![a.id, a.path, a.name, a.mime, a.width, a.height, a.created_at],
        )?;
        Ok(())
    }

    pub fn asset(&self, id: &str) -> Result<Option<Asset>> {
        let c = self.conn.lock();
        Ok(c.query_row(
            "SELECT id, path, name, mime, width, height, created_at FROM assets WHERE id = ?1",
            [id],
            |r| {
                Ok(Asset {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    name: r.get(2)?,
                    mime: r.get(3)?,
                    width: r.get(4)?,
                    height: r.get(5)?,
                    created_at: r.get(6)?,
                })
            },
        )
        .optional()?)
    }
}

fn json_col<T: serde::de::DeserializeOwned>(r: &Row, col: &str) -> rusqlite::Result<T> {
    let s: String = r.get(col)?;
    serde_json::from_str(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn job_from_row(r: &Row) -> rusqlite::Result<Job> {
    let metrics: Option<String> = r.get("metrics")?;
    let status: String = r.get("status")?;
    Ok(Job {
        id: r.get("id")?,
        spec: json_col(r, "spec")?,
        status: JobStatus::parse(&status).unwrap_or(JobStatus::Failed),
        position: r.get("position")?,
        created_at: r.get("created_at")?,
        started_at: r.get("started_at")?,
        finished_at: r.get("finished_at")?,
        error: r.get("error")?,
        outputs: json_col(r, "outputs")?,
        metrics: metrics.and_then(|m| serde_json::from_str(&m).ok()),
    })
}

fn output_from_row(r: &Row) -> rusqlite::Result<Output> {
    Ok(Output {
        id: r.get("id")?,
        job_id: r.get("job_id")?,
        path: r.get("path")?,
        thumb: r.get("thumb")?,
        media: r.get("media")?,
        width: r.get("width")?,
        height: r.get("height")?,
        model: r.get("model")?,
        op: r.get("op")?,
        prompt: r.get("prompt")?,
        seed: r.get("seed")?,
        created_at: r.get("created_at")?,
        favorite: r.get("favorite")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(model: &str) -> JobSpec {
        serde_json::from_value(json!({"model": model, "op": "txt2img", "values": {"prompt": "x"}}))
            .unwrap()
    }

    #[test]
    fn job_lifecycle_and_reorder() {
        let s = Store::in_memory().unwrap();
        s.insert_job("a", &spec("m1")).unwrap();
        s.insert_job("b", &spec("m2")).unwrap();
        s.insert_job("c", &spec("m1")).unwrap();
        s.reorder(&["c".into(), "a".into()]).unwrap();
        let q: Vec<String> = s.queued().unwrap().into_iter().map(|j| j.id).collect();
        assert_eq!(q, ["c", "a", "b"]);

        s.set_status("c", JobStatus::Running, None).unwrap();
        assert_eq!(
            s.recover().unwrap(),
            1,
            "running job is requeued after a crash"
        );
        s.set_status("c", JobStatus::Running, None).unwrap();
        s.finish_job(
            "c",
            &["o1".into()],
            &RunMetrics {
                seconds: 3.0,
                ..Default::default()
            },
        )
        .unwrap();
        let c = s.job("c").unwrap().unwrap();
        assert_eq!(c.status, JobStatus::Done);
        assert_eq!(c.outputs, ["o1"]);
        assert!(c.finished_at.is_some());
        assert_eq!(s.jobs(10).unwrap().len(), 3);
    }

    #[test]
    fn outputs_search_escapes_wildcards() {
        let s = Store::in_memory().unwrap();
        for (i, p) in ["a red fox", "100% blue", "green_tea"].iter().enumerate() {
            s.insert_output(&Output {
                id: format!("o{i}"),
                job_id: "j".into(),
                path: format!("outputs/{i}.png"),
                thumb: None,
                media: "image".into(),
                width: Some(64),
                height: Some(64),
                model: "m".into(),
                op: "txt2img".into(),
                prompt: p.to_string(),
                seed: Some(1),
                created_at: i as i64,
                favorite: false,
            })
            .unwrap();
        }
        let q = |s2: &str| {
            s.outputs(&OutputQuery {
                search: Some(s2.into()),
                ..Default::default()
            })
            .unwrap()
            .len()
        };
        assert_eq!(q("fox"), 1);
        assert_eq!(q("%"), 1, "% is literal");
        assert_eq!(q("_"), 1, "_ is literal");
        s.set_favorite("o2", true).unwrap();
        assert_eq!(
            s.outputs(&OutputQuery {
                favorites: Some(true),
                ..Default::default()
            })
            .unwrap()[0]
                .id,
            "o2"
        );
        assert_eq!(
            s.outputs(&Default::default()).unwrap()[0].id,
            "o2",
            "newest first"
        );
    }

    #[test]
    fn settings_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        s.set_setting("vram_headroom_gb", &json!(1.5)).unwrap();
        s.set_setting("vram_headroom_gb", &json!(2)).unwrap();
        assert_eq!(s.setting("vram_headroom_gb").unwrap(), Some(json!(2)));
        assert_eq!(s.settings().unwrap().len(), 1);
    }
}
