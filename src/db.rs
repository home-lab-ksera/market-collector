//! 共用 PostgreSQL への接続、構成銘柄の同期、日足の upsert。
use crate::{config::Market, constituents::Constituent, yahoo::Chart};
use anyhow::{Result, bail};
use jiff::civil::Date;
use sqlx::{
    ConnectOptions, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions, PgSslMode},
};
use std::{env, time::Duration};

pub async fn connect() -> Result<PgPool> {
    let options = if let Ok(url) = env::var("DATABASE_URL") {
        url.parse::<PgConnectOptions>()?
    } else {
        if env::var_os("PGDATABASE").is_none() {
            bail!("PGDATABASE or DATABASE_URL is required");
        }
        PgConnectOptions::new()
            .host(&env::var("PGHOST")?)
            .username(&env::var("PGUSER").unwrap_or_else(|_| "app".into()))
            .database(&env::var("PGDATABASE")?)
            .password(&env::var("PGPASSWORD")?)
            .ssl_mode(PgSslMode::VerifyFull)
            .ssl_root_cert(env::var("PGSSLROOTCERT")?)
    };
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(
            options
                .application_name("market-collector")
                .disable_statement_logging(),
        )
        .await?;
    Ok(pool)
}

pub async fn migrate(pool: &PgPool) -> Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// 現在の構成銘柄と DB の有効な期間を突き合わせる。新しい銘柄は期間を開き、消えた銘柄は `today` で閉じる。
/// 社名が変わった銘柄は名前だけ更新する。1 トランザクションで行う。
pub async fn sync_memberships(
    pool: &PgPool,
    market: Market,
    current: &[Constituent],
    today: Date,
) -> Result<SyncReport> {
    let symbols: Vec<&str> = current.iter().map(|c| c.symbol.as_str()).collect();
    let names: Vec<&str> = current.iter().map(|c| c.name.as_str()).collect();
    let mut tx = pool.begin().await?;
    // 2 つのジョブが同じ指数を同時に同期しないようにする。
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('market-collector:' || $1))")
        .bind(market.as_str())
        .execute(&mut *tx)
        .await?;
    let removed: Vec<String> = sqlx::query_scalar(
        "UPDATE index_memberships SET removed_on = $3::date
         WHERE market = $1 AND removed_on IS NULL AND NOT (symbol = ANY($2))
         RETURNING symbol",
    )
    .bind(market.as_str())
    .bind(&symbols)
    .bind(today.to_string())
    .fetch_all(&mut *tx)
    .await?;
    let added: Vec<String> = sqlx::query_scalar(
        "INSERT INTO index_memberships (market, symbol, name, added_on)
         SELECT $1, c.symbol, c.name, $4::date FROM unnest($2::text[], $3::text[]) AS c(symbol, name)
         WHERE NOT EXISTS (
             SELECT FROM index_memberships m
             WHERE m.market = $1 AND m.symbol = c.symbol AND m.removed_on IS NULL)
         RETURNING symbol",
    )
    .bind(market.as_str())
    .bind(&symbols)
    .bind(&names)
    .bind(today.to_string())
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE index_memberships m SET name = c.name
         FROM unnest($2::text[], $3::text[]) AS c(symbol, name)
         WHERE m.market = $1 AND m.symbol = c.symbol AND m.removed_on IS NULL AND m.name <> c.name",
    )
    .bind(market.as_str())
    .bind(&symbols)
    .bind(&names)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut report = SyncReport { added, removed };
    report.added.sort();
    report.removed.sort();
    Ok(report)
}

/// 有効な構成銘柄と、日足がすでに保存されているか。
pub async fn active_members(pool: &PgPool, market: Market) -> Result<Vec<(String, bool)>> {
    Ok(sqlx::query_as(
        "SELECT m.symbol, EXISTS (SELECT FROM daily_prices p WHERE p.symbol = m.symbol)
         FROM index_memberships m
         WHERE m.market = $1 AND m.removed_on IS NULL
         ORDER BY m.symbol",
    )
    .bind(market.as_str())
    .fetch_all(pool)
    .await?)
}

/// 取得した日足を書き込む。値が変わった行だけ更新し、書いた行数を返す。
pub async fn upsert_prices(pool: &PgPool, symbol: &str, chart: &Chart) -> Result<u64> {
    let dates: Vec<String> = chart.bars.iter().map(|b| b.date.to_string()).collect();
    let closes: Vec<f64> = chart.bars.iter().map(|b| b.close).collect();
    let volumes: Vec<i64> = chart.bars.iter().map(|b| b.volume).collect();
    let result = sqlx::query(
        "INSERT INTO daily_prices (symbol, trade_date, currency, close, volume)
         SELECT $1, d::date, $2, round(c::numeric, 4), v
         FROM unnest($3::text[], $4::float8[], $5::int8[]) AS t(d, c, v)
         ON CONFLICT (symbol, trade_date) DO UPDATE
         SET close = EXCLUDED.close, volume = EXCLUDED.volume, currency = EXCLUDED.currency, fetched_at = now()
         WHERE (daily_prices.close, daily_prices.volume, daily_prices.currency)
             IS DISTINCT FROM (EXCLUDED.close, EXCLUDED.volume, EXCLUDED.currency)",
    )
    .bind(symbol)
    .bind(&chart.currency)
    .bind(&dates)
    .bind(&closes)
    .bind(&volumes)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yahoo::DailyBar;

    // TEST_DATABASE_URL の DB で実行する: cargo test -- --ignored --test-threads=1
    async fn pool() -> PgPool {
        let url = env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
        let pool = PgPool::connect(&url).await.unwrap();
        migrate(&pool).await.unwrap();
        pool
    }

    fn c(symbol: &str, name: &str) -> Constituent {
        Constituent {
            symbol: symbol.into(),
            name: name.into(),
        }
    }

    fn d(s: &str) -> Date {
        s.parse().unwrap()
    }

    #[tokio::test]
    #[ignore]
    async fn sync_tracks_additions_removals_and_readditions() {
        let pool = pool().await;
        sqlx::query("DELETE FROM index_memberships WHERE symbol LIKE 'TST%'")
            .execute(&pool)
            .await
            .unwrap();
        let m = Market::Sp500;
        let r = sync_memberships(&pool, m, &[c("TSTA", "A"), c("TSTB", "B")], d("2026-01-05"))
            .await
            .unwrap();
        assert_eq!(r.added, ["TSTA", "TSTB"]);
        // 同じ一覧なら何も変わらない。社名だけの変更は期間を切らない。
        let r = sync_memberships(
            &pool,
            m,
            &[c("TSTA", "A2"), c("TSTB", "B")],
            d("2026-01-06"),
        )
        .await
        .unwrap();
        assert_eq!(r, SyncReport::default());
        let r = sync_memberships(
            &pool,
            m,
            &[c("TSTA", "A2"), c("TSTC", "C")],
            d("2026-03-23"),
        )
        .await
        .unwrap();
        assert_eq!(
            (r.added, r.removed),
            (vec!["TSTC".into()], vec!["TSTB".into()])
        );
        let r = sync_memberships(
            &pool,
            m,
            &[c("TSTA", "A2"), c("TSTB", "B"), c("TSTC", "C")],
            d("2026-06-22"),
        )
        .await
        .unwrap();
        assert_eq!(r.added, ["TSTB"]);
        let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT symbol, name, added_on::text, removed_on::text FROM index_memberships
             WHERE symbol LIKE 'TST%' ORDER BY symbol, added_on",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let s = |a: &str, b: &str, e: &str, r: Option<&str>| {
            (a.into(), b.into(), e.into(), r.map(Into::into))
        };
        assert_eq!(
            rows,
            [
                s("TSTA", "A2", "2026-01-05", None),
                s("TSTB", "B", "2026-01-05", Some("2026-03-23")),
                s("TSTB", "B", "2026-06-22", None),
                s("TSTC", "C", "2026-03-23", None),
            ]
        );
        // 別の指数の同期は影響しない。
        let r = sync_memberships(
            &pool,
            Market::Nikkei225,
            &[c("TSTX.T", "X")],
            d("2026-06-22"),
        )
        .await
        .unwrap();
        assert!(r.removed.is_empty());
        let active = active_members(&pool, m).await.unwrap();
        let active: Vec<_> = active
            .iter()
            .filter(|(s, _)| s.starts_with("TST"))
            .collect();
        assert_eq!(active.len(), 3);
    }

    #[tokio::test]
    #[ignore]
    async fn upsert_is_idempotent_and_updates_changed_rows() {
        let pool = pool().await;
        sqlx::query("DELETE FROM daily_prices WHERE symbol = 'TEST.T'")
            .execute(&pool)
            .await
            .unwrap();
        let chart = |close| Chart {
            currency: "JPY".into(),
            bars: vec![
                DailyBar {
                    date: d("2026-11-17"),
                    close: 3414.0,
                    volume: 100,
                },
                DailyBar {
                    date: d("2026-11-18"),
                    close,
                    volume: 200,
                },
            ],
        };
        assert_eq!(
            upsert_prices(&pool, "TEST.T", &chart(3378.123456))
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            upsert_prices(&pool, "TEST.T", &chart(3378.123456))
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            upsert_prices(&pool, "TEST.T", &chart(3380.0))
                .await
                .unwrap(),
            1
        );
        let close: String = sqlx::query_scalar(
            "SELECT close::text FROM daily_prices WHERE symbol = 'TEST.T' AND trade_date = '2026-11-18'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(close, "3380.0000");
    }
}
