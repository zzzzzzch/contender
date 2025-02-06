use alloy::{
    hex::{FromHex, ToHexExt},
    primitives::{Address, TxHash},
};
use contender_core::db::{DbOps, NamedTx, RunTx, SpamRun};
use contender_core::{error::ContenderError, Result};
use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, types::FromSql, Row};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct SqliteDb {
    pool: Pool<SqliteConnectionManager>,
}

impl SqliteDb {
    pub fn from_file(file: &str) -> Result<Self> {
        let manager = SqliteConnectionManager::file(file);
        let pool = Pool::new(manager).map_err(|e| {
            ContenderError::DbError("failed to create connection pool", Some(e.to_string()))
        })?;
        Ok(Self { pool })
    }

    pub fn new_memory() -> Self {
        let manager = SqliteConnectionManager::memory();
        let pool = Pool::new(manager).expect("failed to create connection pool");
        Self { pool }
    }

    fn get_pool(&self) -> Result<PooledConnection<SqliteConnectionManager>> {
        self.pool.get().map_err(|e| {
            ContenderError::DbError("failed to get connection from pool", Some(e.to_string()))
        })
    }

    fn execute<P: rusqlite::Params>(&self, query: &str, params: P) -> Result<()> {
        self.get_pool()?
            .execute(query, params)
            .map_err(|e| ContenderError::DbError("failed to execute query", Some(e.to_string())))?;
        Ok(())
    }

    fn query_row<
        T: FromSql,
        P: rusqlite::Params,
        F: FnOnce(&Row<'_>) -> std::result::Result<T, rusqlite::Error>,
    >(
        &self,
        query: &str,
        params: P,
        with_row: F,
    ) -> Result<T> {
        self.get_pool()?
            .query_row(query, params, with_row)
            .map_err(|e| ContenderError::DbError("failed to query row", Some(e.to_string())))
    }
}

#[derive(Deserialize, Debug, Serialize)]
struct NamedTxRow {
    name: String,
    tx_hash: String,
    contract_address: Option<String>,
}

impl From<NamedTxRow> for NamedTx {
    fn from(row: NamedTxRow) -> Self {
        let tx_hash = TxHash::from_hex(&row.tx_hash).expect("invalid tx hash");
        let contract_address = row
            .contract_address
            .map(|a| Address::from_hex(&a))
            .transpose()
            .expect("invalid address");
        NamedTx::new(row.name, tx_hash, contract_address)
    }
}

impl NamedTxRow {
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            name: row.get(0)?,
            tx_hash: row.get(1)?,
            contract_address: row.get(2)?,
        })
    }
}

#[derive(Deserialize, Debug, Serialize)]
struct RunTxRow {
    run_id: i64,
    tx_hash: String,
    start_timestamp: usize,
    end_timestamp: usize,
    block_number: u64,
    gas_used: String,
    kind: Option<String>,
}

impl RunTxRow {
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            run_id: row.get(0)?,
            tx_hash: row.get(1)?,
            start_timestamp: row.get(2)?,
            end_timestamp: row.get(3)?,
            block_number: row.get(4)?,
            gas_used: row.get(5)?,
            kind: row.get(6)?,
        })
    }
}

impl From<RunTxRow> for RunTx {
    fn from(row: RunTxRow) -> Self {
        let tx_hash = TxHash::from_hex(&row.tx_hash).expect("invalid tx hash");
        Self {
            tx_hash,
            start_timestamp: row.start_timestamp,
            end_timestamp: row.end_timestamp,
            block_number: row.block_number,
            gas_used: row.gas_used.parse().expect("invalid gas_used parameter"),
            kind: row.kind,
        }
    }
}

struct SpamRunRow {
    pub id: u64,
    pub timestamp: String,
    pub tx_count: usize,
    pub scenario_name: String,
}

impl From<SpamRunRow> for SpamRun {
    fn from(row: SpamRunRow) -> Self {
        Self {
            id: row.id,
            timestamp: row.timestamp.parse::<usize>().expect("invalid timestamp"),
            tx_count: row.tx_count,
            scenario_name: row.scenario_name,
        }
    }
}

impl DbOps for SqliteDb {
    fn create_tables(&self) -> Result<()> {
        let ignore_already_exists = |e: ContenderError| {
            let err_str = format!("{:?}", e);
            if err_str.contains("already exists") || err_str.contains("duplicate column name") {
                Ok(())
            } else {
                Err(e)
            }
        };

        let queries = [
            self.execute(
                "CREATE TABLE runs (
                    id INTEGER PRIMARY KEY,
                    timestamp TEXT NOT NULL,
                    tx_count INTEGER NOT NULL
                )",
                params![],
            ),
            self.execute(
                "CREATE TABLE rpc_urls (
                    id INTEGER PRIMARY KEY,
                    url TEXT NOT NULL UNIQUE
                )",
                params![],
            ),
            self.execute(
                "CREATE TABLE named_txs (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL,
                    tx_hash TEXT NOT NULL,
                    contract_address TEXT,
                    rpc_url_id INTEGER NOT NULL,
                    FOREIGN KEY (rpc_url_id) REFERENCES rpc_urls(id)
                )",
                params![],
            ),
            self.execute(
                "CREATE TABLE run_txs (
                    id INTEGER PRIMARY KEY,
                    run_id INTEGER NOT NULL,
                    tx_hash TEXT NOT NULL,
                    start_timestamp INTEGER NOT NULL,
                    end_timestamp INTEGER NOT NULL,
                    block_number INTEGER NOT NULL,
                    gas_used TEXT NOT NULL,
                    kind TEXT,
                    FOREIGN KEY(run_id) REFERENCES runs(runid)
                )",
                params![],
            ),
            self.execute(
                "ALTER TABLE runs ADD COLUMN scenario_name TEXT NOT NULL DEFAULT '';",
                params![],
            ),
        ];
        for query in queries {
            query.or_else(ignore_already_exists)?;
        }
        Ok(())
    }

    /// Inserts a new run into the database and returns the ID of the new row.
    fn insert_run(&self, timestamp: u64, tx_count: usize, scenario_name: &str) -> Result<u64> {
        self.execute(
            "INSERT INTO runs (timestamp, tx_count, scenario_name) VALUES (?, ?, ?)",
            params![timestamp, tx_count, scenario_name],
        )?;
        // get ID from newly inserted row
        let id: u64 = self.query_row("SELECT last_insert_rowid()", params![], |row| row.get(0))?;
        Ok(id)
    }

    fn num_runs(&self) -> Result<u64> {
        let count: u64 =
            self.query_row("SELECT COUNT(*) FROM runs", params![], |row| row.get(0))?;
        Ok(count)
    }

    fn get_run_txs(&self, run_id: u64) -> Result<Vec<RunTx>> {
        let pool = self.get_pool()?;
        let mut stmt = pool
            .prepare("SELECT run_id, tx_hash, start_timestamp, end_timestamp, block_number, gas_used, kind FROM run_txs WHERE run_id = ?1")
            .map_err(|e| ContenderError::with_err(e, "failed to prepare statement"))?;

        let rows = stmt
            .query_map(params![run_id], RunTxRow::from_row)
            .map_err(|e| ContenderError::with_err(e, "failed to map row"))?;
        let res = rows
            .map(|r| r.map(|r| r.into()))
            .map(|r| r.map_err(|e| ContenderError::with_err(e, "failed to convert row")))
            .collect::<Result<Vec<RunTx>>>()
            .map_err(|e| ContenderError::with_err(e, "failed to collect rows"))?;
        Ok(res)
    }

    fn get_run(&self, run_id: u64) -> Result<Option<SpamRun>> {
        let pool = self.get_pool()?;
        let mut stmt = pool
            .prepare("SELECT id, timestamp, tx_count, scenario_name FROM runs WHERE id = ?1")
            .map_err(|e| ContenderError::with_err(e, "failed to prepare statement"))?;

        let row = stmt
            .query_map(params![run_id], |row| {
                Ok(SpamRunRow {
                    id: row.get(0)?,
                    timestamp: row.get(1)?,
                    tx_count: row.get(2)?,
                    scenario_name: row.get(3)?,
                })
            })
            .map_err(|e| ContenderError::with_err(e, "failed to map row"))?;
        let res = row
            .last()
            .transpose()
            .map_err(|e| ContenderError::with_err(e, "failed to query row"))?;
        Ok(res.map(|r| r.into()))
    }

    fn insert_named_txs(&self, named_txs: Vec<NamedTx>, rpc_url: &str) -> Result<()> {
        let pool = self.get_pool()?;

        // first check the rpc_urls table; insert if not present
        pool.execute(
            "INSERT OR IGNORE INTO rpc_urls (url) VALUES (?)",
            params![rpc_url],
        )
        .map_err(|e| ContenderError::with_err(e, "failed to insert rpc_url into DB"))?;

        // then get the rpc_url ID
        let rpc_url_id: i64 = self.query_row(
            "SELECT id FROM rpc_urls WHERE url = ?1",
            params![rpc_url],
            |row| row.get(0),
        )?;

        let stmts = named_txs.iter().map(|tx| {
            format!(
                "INSERT INTO named_txs (name, tx_hash, contract_address, rpc_url_id) VALUES ('{}', '{}', '{}', {});",
                tx.name,
                tx.tx_hash.encode_hex(),
                tx.address.map(|a| a.encode_hex()).unwrap_or_default(),
                rpc_url_id,
            )
        });
        pool.execute_batch(&format!(
            "BEGIN;
            {}
            COMMIT;",
            stmts
                .reduce(|ac, c| format!("{}\n{}", ac, c))
                .unwrap_or_default(),
        ))
        .map_err(|e| ContenderError::with_err(e, "failed to execute batch"))?;
        Ok(())
    }

    fn get_named_tx(&self, name: &str, rpc_url: &str) -> Result<Option<NamedTx>> {
        let pool = self.get_pool()?;
        let mut stmt = pool
            .prepare(
                "SELECT name, tx_hash, contract_address, rpc_url_id FROM named_txs WHERE name = ?1 AND rpc_url_id = (
                    SELECT id FROM rpc_urls WHERE url = ?2
                ) ORDER BY id DESC LIMIT 1",
            )
            .map_err(|e| ContenderError::with_err(e, "failed to prepare statement"))?;

        let row = stmt
            .query_map(params![name, rpc_url], NamedTxRow::from_row)
            .map_err(|e| ContenderError::with_err(e, "failed to map row"))?;
        let res = row
            .last()
            .transpose()
            .map_err(|e| ContenderError::with_err(e, "failed to query row"))?
            .map(|r| r.into());
        Ok(res)
    }

    fn get_named_tx_by_address(&self, address: &Address) -> Result<Option<NamedTx>> {
        let pool = self.get_pool()?;
        let mut stmt = pool
            .prepare(
                "SELECT name, tx_hash, contract_address FROM named_txs WHERE contract_address = ?1 ORDER BY id DESC LIMIT 1",
            )
            .map_err(|e| ContenderError::with_err(e, "failed to prepare statement"))?;

        let row = stmt
            .query_map(params![address.encode_hex()], |row| {
                NamedTxRow::from_row(row)
            })
            .map_err(|e| ContenderError::with_err(e, "failed to map row query"))?;
        let res = row
            .last()
            .transpose()
            .map_err(|e| ContenderError::with_err(e, "failed to query row"))?
            .map(|r| r.into());
        Ok(res)
    }

    fn insert_run_txs(&self, run_id: u64, run_txs: Vec<RunTx>) -> Result<()> {
        let pool = self.get_pool()?;
        let stmts = run_txs.iter().map(|tx| {
            if let Some(kind) = &tx.kind {
                format!(
                    "INSERT INTO run_txs (run_id, tx_hash, start_timestamp, end_timestamp, block_number, gas_used, kind) VALUES ({}, '{}', {}, {}, {}, '{}', '{}');",
                    run_id,
                    tx.tx_hash.encode_hex(),
                    tx.start_timestamp,
                    tx.end_timestamp,
                    tx.block_number,
                    tx.gas_used,
                    kind,
                )
            } else {
                format!(
                    "INSERT INTO run_txs (run_id, tx_hash, start_timestamp, end_timestamp, block_number, gas_used) VALUES ({}, '{}', {}, {}, {}, '{}');",
                    run_id,
                    tx.tx_hash.encode_hex(),
                    tx.start_timestamp,
                    tx.end_timestamp,
                    tx.block_number,
                    tx.gas_used,
                )
            }
        });
        pool.execute_batch(&format!(
            "BEGIN;
            {}
            COMMIT;",
            stmts
                .reduce(|ac, c| format!("{}\n{}", ac, c))
                .unwrap_or_default(),
        ))
        .map_err(|e| ContenderError::with_err(e, "failed to execute batch"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_table() {
        let db = SqliteDb::new_memory();
        db.create_tables().unwrap();
        assert_eq!(db.num_runs().unwrap(), 0);
    }

    #[test]
    fn inserts_runs() {
        let db = SqliteDb::new_memory();
        db.create_tables().unwrap();
        let do_it = |num| db.insert_run(100000, num, "test").unwrap();

        println!("id: {}", do_it(100));
        println!("id: {}", do_it(101));
        println!("id: {}", do_it(102));
        assert_eq!(db.num_runs().unwrap(), 3);
    }

    #[test]
    fn inserts_and_gets_named_txs() {
        let db = SqliteDb::new_memory();
        db.create_tables().unwrap();
        let tx_hash = TxHash::from_slice(&[0u8; 32]);
        let contract_address = Some(Address::from_slice(&[4u8; 20]));
        let name1 = "test_tx".to_string();
        let name2 = "test_tx2";
        let rpc_url = "http://test.url:8545";
        db.insert_named_txs(
            vec![
                NamedTx::new(name1.to_owned(), tx_hash, contract_address),
                NamedTx::new(name2.to_string(), tx_hash, contract_address),
            ],
            rpc_url,
        )
        .unwrap();
        let count: i64 = db
            .get_pool()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM named_txs", params![], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 2);

        let res1 = db.get_named_tx(&name1, rpc_url).unwrap().unwrap();
        assert_eq!(res1.name, name1);
        assert_eq!(res1.tx_hash, tx_hash);
        assert_eq!(res1.address, contract_address);
        let res2 = db.get_named_tx(&name1, "http://wrong.url:8545").unwrap();
        assert!(res2.is_none());
    }

    #[test]
    fn inserts_and_gets_run_txs() {
        let db = SqliteDb::new_memory();
        db.create_tables().unwrap();
        let run_id = db.insert_run(100000, 100, "test").unwrap();
        let run_txs = vec![
            RunTx {
                tx_hash: TxHash::from_slice(&[0u8; 32]),
                start_timestamp: 100,
                end_timestamp: 200,
                block_number: 1,
                gas_used: 100,
                kind: Some("test".to_string()),
            },
            RunTx {
                tx_hash: TxHash::from_slice(&[1u8; 32]),
                start_timestamp: 200,
                end_timestamp: 300,
                block_number: 2,
                gas_used: 200,
                kind: Some("test".to_string()),
            },
        ];
        db.insert_run_txs(run_id, run_txs).unwrap();
        let count: i64 = db
            .get_pool()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM run_txs", params![], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);

        let res = db.get_run_txs(run_id).unwrap();
        assert_eq!(res.len(), 2);
    }
}
