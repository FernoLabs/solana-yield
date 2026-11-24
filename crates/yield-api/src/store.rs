use crate::{
    config::{Config, SourceConfig},
    model::Observation,
};
use anyhow::{ensure, Result};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

pub struct Store {
    db: Connection,
    failed_sources: std::collections::BTreeSet<String>,
}
fn sql(value: u64) -> Result<i64> {
    Ok(i64::try_from(value)?)
}

impl Store {
    pub fn open(path: &str) -> Result<Self> {
        if path != ":memory:" {
            if let Some(parent) = std::path::Path::new(path)
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent)?;
            }
        }
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS sources(id TEXT PRIMARY KEY,adapter TEXT NOT NULL,origin TEXT NOT NULL,status TEXT NOT NULL,last_attempt_ms INTEGER,last_success_ms INTEGER,error TEXT);
          CREATE TABLE IF NOT EXISTS observations(seq INTEGER PRIMARY KEY AUTOINCREMENT,source_id TEXT NOT NULL,id TEXT NOT NULL,collected_at_ms INTEGER NOT NULL,payload TEXT NOT NULL);
          CREATE INDEX IF NOT EXISTS observation_lookup ON observations(source_id,id,seq DESC);
          CREATE INDEX IF NOT EXISTS observation_time ON observations(collected_at_ms);
          CREATE TABLE IF NOT EXISTS latest(source_id TEXT NOT NULL,id TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(source_id,id));")?;
        Ok(Self {
            db,
            failed_sources: Default::default(),
        })
    }

    pub fn configure(&mut self, config: &Config) -> Result<()> {
        let tx = self.db.transaction()?;
        // Each process must verify its configured authorities before serving rates.
        // Historical observations remain available with their original provenance.
        tx.execute("DELETE FROM latest", [])?;
        tx.execute("DELETE FROM sources", [])?;
        for source in &config.sources {
            tx.execute(
                "INSERT INTO sources(id,adapter,origin,status) VALUES(?1,?2,?3,'pending')",
                params![
                    source.id(),
                    source.adapter(),
                    if source.is_api() { "api" } else { "onchain" }
                ],
            )?;
        }
        tx.commit()?;
        self.failed_sources.clear();
        Ok(())
    }

    pub fn ingest(&mut self, source: &SourceConfig, values: &[Observation], at: u64) -> Result<()> {
        ensure!(values.len() <= 20_000, "source entity limit exceeded");
        let mut values = values.to_vec();
        let mut ids = std::collections::BTreeSet::new();
        for value in &mut values {
            value.normalize_protocols();
            value.validate()?;
            ensure!(
                value.source_id == source.id(),
                "observation source mismatch"
            );
            ensure!(
                value.collected_at_ms > 0
                    && value.collected_at_ms <= at
                    && at.saturating_sub(value.collected_at_ms) <= 60_000,
                "invalid collection time"
            );
            ensure!(
                (value.evidence.origin == crate::model::Origin::Api) == source.is_api(),
                "observation authority mismatch"
            );
            ensure!(
                ids.insert(value.id.as_str()),
                "duplicate observation identity"
            );
        }
        let tx = self.db.transaction()?;
        // Absence from a successful complete API response must clear old current rows.
        tx.execute("DELETE FROM latest WHERE source_id=?1", [source.id()])?;
        for value in &values {
            let payload = serde_json::to_string(value)?;
            tx.execute("INSERT OR REPLACE INTO observations(source_id,id,collected_at_ms,payload) VALUES(?1,?2,?3,?4)", params![source.id(), value.id, sql(value.collected_at_ms)?, payload])?;
            tx.execute(
                "INSERT INTO latest(source_id,id,payload) VALUES(?1,?2,?3)",
                params![source.id(), value.id, payload],
            )?;
        }
        let count: i64 = tx.query_row("SELECT count(*) FROM latest", [], |row| row.get(0))?;
        ensure!(count <= 25_000, "global current entity limit exceeded");
        let status = if !values.is_empty()
            && values
                .iter()
                .all(|value| value.unavailable_reason.is_none())
        {
            "ok"
        } else {
            "unavailable"
        };
        tx.execute("UPDATE sources SET status=?2,last_attempt_ms=?3,last_success_ms=?3,error=NULL WHERE id=?1", params![source.id(), status, sql(at)?])?;
        tx.commit()?;
        self.failed_sources.remove(source.id());
        Ok(())
    }

    pub fn fail(&mut self, source: &SourceConfig, at: u64) -> Result<()> {
        // Keep failed rates unavailable even if the database cannot persist a tombstone.
        self.failed_sources.insert(source.id().to_owned());
        let old = self.read_latest(Some(source.id()))?;
        let tx = self.db.transaction()?;
        for mut value in old {
            value.rates.clear();
            value.unavailable_reason = Some("collection_failed".into());
            value.collected_at_ms = at;
            let payload = serde_json::to_string(&value)?;
            tx.execute(
                "UPDATE latest SET payload=?3 WHERE source_id=?1 AND id=?2",
                params![source.id(), value.id, payload],
            )?;
            tx.execute("INSERT OR REPLACE INTO observations(source_id,id,collected_at_ms,payload) VALUES(?1,?2,?3,?4)", params![source.id(), value.id, sql(at)?, payload])?;
        }
        tx.execute("UPDATE sources SET status='error',last_attempt_ms=?2,error='collection_failed' WHERE id=?1", params![source.id(), sql(at)?])?;
        tx.commit()?;
        Ok(())
    }

    fn read_latest(&self, source: Option<&str>) -> Result<Vec<Observation>> {
        let mut query = self.db.prepare(
            "SELECT payload FROM latest WHERE (?1 IS NULL OR source_id=?1) ORDER BY source_id,id",
        )?;
        let rows = query.query_map([source], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn latest(
        &self,
        now: u64,
        max_age_ms: u64,
        origin: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<Value> {
        self.latest_for_protocol(now, max_age_ms, origin, None, offset, limit)
    }

    /// Filter the full source and protocol set before counting or paging.
    pub fn latest_for_protocol(
        &self,
        now: u64,
        max_age_ms: u64,
        origin: Option<&str>,
        protocol: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<Value> {
        let values = self.read_latest(None)?;
        let mut count = 0;
        let mut output = Vec::new();
        for mut value in values {
            let name = if value.evidence.origin == crate::model::Origin::Api {
                "api"
            } else {
                "onchain"
            };
            if origin.is_some_and(|filter| filter != name)
                || protocol.is_some_and(|filter| {
                    let filter = crate::model::canonical_protocol(filter);
                    !value.protocols.iter().any(|name| {
                        crate::model::canonical_protocol(name).eq_ignore_ascii_case(filter)
                    })
                })
            {
                continue;
            }
            if count >= offset && output.len() < limit {
                if self.failed_sources.contains(&value.source_id) {
                    value.rates.clear();
                    value.unavailable_reason = Some("collection_failed".into());
                }
                if value.unavailable_reason.is_none()
                    && value.observed_at_ms.is_none_or(|at| {
                        now.saturating_sub(at) > max_age_ms || at > now.saturating_add(30_000)
                    })
                {
                    value.rates.clear();
                    value.unavailable_reason = Some("source_stale".into());
                }
                output.push(value);
            }
            count += 1;
        }
        Ok(json!({"schema_version":1,"total":count,"offset":offset,"observations":output}))
    }

    pub fn sources(&self) -> Result<Value> {
        let mut statement = self.db.prepare("SELECT id,adapter,origin,status,last_attempt_ms,last_success_ms,error FROM sources ORDER BY id")?;
        let mut values = statement.query_map([], |row| Ok(json!({"id":row.get::<_,String>(0)?,"adapter":row.get::<_,String>(1)?,"origin":row.get::<_,String>(2)?,"status":row.get::<_,String>(3)?,"last_attempt_ms":row.get::<_,Option<i64>>(4)?,"last_success_ms":row.get::<_,Option<i64>>(5)?,"error":row.get::<_,Option<String>>(6)?})))?.collect::<Result<Vec<_>,_>>()?;
        for value in &mut values {
            if self
                .failed_sources
                .contains(value["id"].as_str().unwrap_or(""))
            {
                value["status"] = json!("error");
                value["error"] = json!("collection_failed");
            }
        }
        Ok(json!({"sources":values}))
    }

    pub fn coverage(&self, now: u64, age: u64) -> Result<Value> {
        let current = self.latest(now, age, None, 0, 25_000)?;
        let rows = current["observations"].as_array().expect("array");
        let native = rows
            .iter()
            .filter(|row| row["evidence"]["origin"] == "onchain")
            .count();
        let available = rows
            .iter()
            .filter(|row| row["unavailable_reason"].is_null())
            .count();
        let sources = self.sources()?;
        let source_rows = sources["sources"].as_array().expect("array");
        let ready = !source_rows.is_empty()
            && source_rows.iter().all(|row| row["status"] == "ok")
            && !rows.is_empty()
            && available == rows.len();
        let protocols: std::collections::BTreeSet<_> = rows
            .iter()
            .flat_map(|row| row["protocols"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .collect();
        let positions: std::collections::BTreeSet<_> =
            rows.iter().filter_map(|row| row["id"].as_str()).collect();
        Ok(
            json!({"ready":ready,"configured_sources":source_rows.len(),"positions":positions.len(),"observations":rows.len(),"onchain":native,"api":rows.len()-native,"available":available,"unavailable":rows.len()-available,"protocols":protocols }),
        )
    }

    pub fn history(&self, source: &str, id: &str, limit: usize) -> Result<Value> {
        let mut query = self.db.prepare("SELECT payload FROM observations WHERE source_id=?1 AND id=?2 ORDER BY seq DESC LIMIT ?3")?;
        let rows = query.query_map(params![source, id, i64::try_from(limit)?], |row| {
            row.get::<_, String>(0)
        })?;
        let values = rows
            .map(|row| Ok(serde_json::from_str::<Observation>(&row?)?))
            .collect::<Result<Vec<_>>>()?;
        Ok(json!({"schema_version":1,"source_id":source,"id":id,"observations":values}))
    }

    pub fn prune(&mut self, now: u64, days: u64, maximum: u64) -> Result<()> {
        self.db.execute(
            "DELETE FROM observations WHERE collected_at_ms<?1",
            [sql(now.saturating_sub(days.saturating_mul(86_400_000)))?],
        )?;
        self.db.execute("DELETE FROM observations WHERE seq<=COALESCE((SELECT seq FROM observations ORDER BY seq DESC LIMIT 1 OFFSET ?1),0)", [sql(maximum)?])?;
        Ok(())
    }
}
