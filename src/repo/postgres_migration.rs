use crate::repo::postgres::PostgresPool;
use async_trait::async_trait;
use sqlx::{Executor, Postgres, Transaction};

#[async_trait]
pub trait Migration {
    fn serial_number(&self) -> i64;
    async fn run(&self, tx: &mut Transaction<Postgres>);
}

struct SimpleSqlMigration {
    pub serial_number: i64,
    pub sql: Vec<&'static str>,
}

#[async_trait]
impl Migration for SimpleSqlMigration {
    fn serial_number(&self) -> i64 {
        self.serial_number
    }

    async fn run(&self, tx: &mut Transaction<Postgres>) {
        for sql in self.sql.iter() {
            tx.execute(*sql).await.unwrap();
        }
    }
}

/// Execute all migrations on the database.
pub async fn run_migrations(db: &PostgresPool) -> crate::error::Result<usize> {
    prepare_migrations_table(db).await;
    run_migration(m001::migration(), db).await;
    let m002_result = run_migration(m002::migration(), db).await;
    if m002_result == MigrationResult::Upgraded {
        m002::rebuild_tags(db).await?;
    }
    run_migration(m003::migration(), db).await;
    run_migration(m004::migration(), db).await;
    Ok(current_version(db).await as usize)
}

async fn current_version(db: &PostgresPool) -> i64 {
    sqlx::query_scalar("SELECT max(serial_number) FROM migrations;")
        .fetch_one(db)
        .await
        .unwrap()
}

async fn prepare_migrations_table(db: &PostgresPool) {
    sqlx::query("CREATE TABLE IF NOT EXISTS migrations (serial_number bigint)")
        .execute(db)
        .await
        .unwrap();
}

// Running a migration was either unnecessary, or completed
#[derive(PartialEq, Eq, Debug, Clone)]
enum MigrationResult {
    Upgraded,
    NotNeeded,
}

async fn run_migration(migration: impl Migration, db: &PostgresPool) -> MigrationResult {
    let row: i64 =
        sqlx::query_scalar("SELECT COUNT(*) AS count FROM migrations WHERE serial_number = $1")
            .bind(migration.serial_number())
            .fetch_one(db)
            .await
            .unwrap();

    if row > 0 {
        return MigrationResult::NotNeeded;
    }

    let mut transaction = db.begin().await.unwrap();
    migration.run(&mut transaction).await;

    sqlx::query("INSERT INTO migrations VALUES ($1)")
        .bind(migration.serial_number())
        .execute(&mut transaction)
        .await
        .unwrap();

    transaction.commit().await.unwrap();
    MigrationResult::Upgraded
}

mod m001 {
    use crate::repo::postgres_migration::{Migration, SimpleSqlMigration};

    pub const VERSION: i64 = 1;

    pub fn migration() -> impl Migration {
        SimpleSqlMigration {
            serial_number: VERSION,
            sql: vec![
                r#"
-- Events table
CREATE TABLE "event" (
	id bytea NOT NULL,
	pub_key bytea NOT NULL,
	created_at timestamp with time zone NOT NULL,
	kind integer NOT NULL,
	"content" bytea NOT NULL,
	hidden bit(1) NOT NULL DEFAULT 0::bit(1),
	delegated_by bytea NULL,
	first_seen timestamp with time zone NOT NULL DEFAULT now(),
	CONSTRAINT event_pkey PRIMARY KEY (id)
);
CREATE INDEX event_created_at_idx ON "event" (created_at,kind);
CREATE INDEX event_pub_key_idx ON "event" (pub_key);
CREATE INDEX event_delegated_by_idx ON "event" (delegated_by);

-- Tags table
CREATE TABLE "tag" (
	id int8 NOT NULL GENERATED BY DEFAULT AS IDENTITY,
	event_id bytea NOT NULL,
	"name" varchar NOT NULL,
	value bytea NOT NULL,
	CONSTRAINT tag_fk FOREIGN KEY (event_id) REFERENCES "event"(id) ON DELETE CASCADE
);
CREATE INDEX tag_event_id_idx ON tag USING btree (event_id, name);
CREATE INDEX tag_value_idx ON tag USING btree (value);

-- NIP-05 Verfication table
CREATE TABLE "user_verification" (
	id int8 NOT NULL GENERATED BY DEFAULT AS IDENTITY,
	event_id bytea NOT NULL,
	"name" varchar NOT NULL,
	verified_at timestamptz NULL,
	failed_at timestamptz NULL,
	fail_count int4 NULL DEFAULT 0,
	CONSTRAINT user_verification_pk PRIMARY KEY (id),
	CONSTRAINT user_verification_fk FOREIGN KEY (event_id) REFERENCES "event"(id) ON DELETE CASCADE
);
CREATE INDEX user_verification_event_id_idx ON user_verification USING btree (event_id);
CREATE INDEX user_verification_name_idx ON user_verification USING btree (name);
        "#,
            ],
        }
    }
}

mod m002 {
    use async_std::stream::StreamExt;
    use indicatif::{ProgressBar, ProgressStyle};
    use sqlx::Row;
    use std::time::Instant;
    use tracing::info;

    use crate::event::{single_char_tagname, Event};
    use crate::repo::postgres::PostgresPool;
    use crate::repo::postgres_migration::{Migration, SimpleSqlMigration};
    use crate::utils::is_lower_hex;

    pub const VERSION: i64 = 2;

    pub fn migration() -> impl Migration {
        SimpleSqlMigration {
            serial_number: VERSION,
            sql: vec![
                r#"
-- Add tag value column
ALTER TABLE tag ADD COLUMN value_hex bytea;
-- Remove not-null constraint
ALTER TABLE tag ALTER COLUMN value DROP NOT NULL;
-- Add value index
CREATE INDEX tag_value_hex_idx ON tag USING btree (value_hex);
        "#,
            ],
        }
    }

    pub async fn rebuild_tags(db: &PostgresPool) -> crate::error::Result<()> {
        // Check how many events we have to process
        let start = Instant::now();
        let mut tx = db.begin().await.unwrap();
        let mut update_tx = db.begin().await.unwrap();
        // Clear out table
        sqlx::query("DELETE FROM tag;")
            .execute(&mut update_tx)
            .await?;
        {
            let event_count: i64 = sqlx::query_scalar("SELECT COUNT(*) from event;")
                .fetch_one(&mut tx)
                .await
                .unwrap();
            let bar = ProgressBar::new(event_count.try_into().unwrap())
                .with_message("rebuilding tags table");
            bar.set_style(
                ProgressStyle::with_template(
                    "[{elapsed_precise}] {bar:40.white/blue} {pos:>7}/{len:7} [{percent}%] {msg}",
                )
                .unwrap(),
            );
            let mut events =
                sqlx::query("SELECT id, content FROM event ORDER BY id;").fetch(&mut tx);
            while let Some(row) = events.next().await {
                bar.inc(1);
                // get the row id and content
                let row = row.unwrap();
                let event_id: Vec<u8> = row.get(0);
                let event_bytes: Vec<u8> = row.get(1);
                let event: Event = serde_json::from_str(&String::from_utf8(event_bytes).unwrap())?;

                for t in event.tags.iter().filter(|x| x.len() > 1) {
                    let tagname = t.get(0).unwrap();
                    let tagnamechar_opt = single_char_tagname(tagname);
                    if tagnamechar_opt.is_none() {
                        continue;
                    }
                    // safe because len was > 1
                    let tagval = t.get(1).unwrap();
                    // insert as BLOB if we can restore it losslessly.
                    // this means it needs to be even length and lowercase.
                    if (tagval.len() % 2 == 0) && is_lower_hex(tagval) {
                        let q = "INSERT INTO tag (event_id, \"name\", value_hex) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING;";
                        sqlx::query(q)
                            .bind(&event_id)
                            .bind(tagname)
                            .bind(hex::decode(tagval).ok())
                            .execute(&mut update_tx)
                            .await?;
                    } else {
                        let q = "INSERT INTO tag (event_id, \"name\", value) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING;";
                        sqlx::query(q)
                            .bind(&event_id)
                            .bind(tagname)
                            .bind(tagval.as_bytes())
                            .execute(&mut update_tx)
                            .await?;
                    }
                }
            }
            update_tx.commit().await?;
            bar.finish();
        }
        info!("rebuilt tags in {:?}", start.elapsed());
        Ok(())
    }
}

mod m003 {
    use crate::repo::postgres_migration::{Migration, SimpleSqlMigration};

    pub const VERSION: i64 = 3;

    pub fn migration() -> impl Migration {
        SimpleSqlMigration {
            serial_number: VERSION,
            sql: vec![
                r#"
-- Add unique constraint on tag
ALTER TABLE tag ADD CONSTRAINT unique_constraint_name UNIQUE (event_id, "name", value);
        "#,
            ],
        }
    }
}

mod m004 {
    use crate::repo::postgres_migration::{Migration, SimpleSqlMigration};

    pub const VERSION: i64 = 4;

    pub fn migration() -> impl Migration {
        SimpleSqlMigration {
            serial_number: VERSION,
            sql: vec![
                r#"
-- Create account table
CREATE TABLE "account" (
    pubkey varchar NOT NULL,
    is_admitted BOOLEAN NOT NULL DEFAULT FALSE,
    balance BIGINT NOT NULL DEFAULT 0,
    tos_accepted_at TIMESTAMP,
    CONSTRAINT account_pkey PRIMARY KEY (pubkey)
);

CREATE TYPE status AS ENUM ('Paid', 'Unpaid', 'Expired');


CREATE TABLE "invoice" (
    payment_hash varchar NOT NULL,
    pubkey varchar NOT NULL,
    amount BIGINT NOT NULL,
    status status NOT NULL DEFAULT 'Unpaid',
    description varchar,
    confirmed_at timestamp,
    created_at timestamp,
    invoice varchar,
    CONSTRAINT invoice_payment_hash PRIMARY KEY (payment_hash),
    CONSTRAINT invoice_pubkey_fkey FOREIGN KEY (pubkey) REFERENCES account (pubkey) ON DELETE CASCADE
);
        "#,
            ],
        }
    }
}
