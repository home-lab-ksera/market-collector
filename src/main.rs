//! 指数の構成銘柄を同期し、有効な銘柄の日足（終値・出来高）を PostgreSQL に upsert する。
//! 対象の指数は `MARKET`（`sp500` か `nikkei225`）で 1 つ選ぶ。
//! 構成銘柄が取れないときは同期も価格の取得もせずに失敗する。
//! 価格は 1 銘柄でも失敗したら、残りを続けたうえで終了コード 1 にする（CronJob の失敗として通知させる）。
use anyhow::{Context, Result};
use jiff::{Timestamp, tz::Offset};
use market_collector::{config::Market, constituents, db, yahoo};
use std::{env, process::ExitCode, time::Duration};

fn env_or(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.into())
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let market = Market::parse(&env::var("MARKET").context("MARKET is required")?)?;
    let default_source = match market {
        Market::Sp500 => constituents::SP500_URL,
        Market::Nikkei225 => constituents::NIKKEI225_URL,
    };
    let source = env_or("CONSTITUENTS_URL", default_source);
    // 取りこぼした日も次の実行で埋まるよう、直近を毎回取り直す。日足の無い銘柄は過去分から取る。
    let lookback = env_or("LOOKBACK_RANGE", "1mo");
    let backfill = env_or("BACKFILL_RANGE", "10y");
    let interval = Duration::from_millis(env_or("REQUEST_INTERVAL_MS", "1000").parse()?);

    let http = reqwest::Client::builder()
        .user_agent(env_or(
            "USER_AGENT",
            "market-collector/0.1 (+https://github.com/home-lab-ksera/market-collector)",
        ))
        .timeout(Duration::from_secs(60))
        .build()?;
    let pool = db::connect().await?;
    db::migrate(&pool).await?;

    let list = constituents::fetch(&http, market, &source)
        .await
        .with_context(|| format!("fetching {} constituents", market.as_str()))?;
    // 検知日は JST の日付で記録する。
    let today = Offset::constant(9).to_datetime(Timestamp::now()).date();
    let report = db::sync_memberships(&pool, market, &list, today).await?;
    println!(
        "{}: {} constituents, added [{}], removed [{}]",
        market.as_str(),
        list.len(),
        report.added.join(", "),
        report.removed.join(", ")
    );

    let yahoo = yahoo::Client::new(
        http,
        env_or("YAHOO_BASE_URL", "https://query1.finance.yahoo.com"),
    );
    let members = db::active_members(&pool, market).await?;
    let mut failed = Vec::new();
    let mut written_total = 0;
    for (i, (symbol, has_prices)) in members.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(interval).await;
        }
        let range = if *has_prices { &lookback } else { &backfill };
        let result = match yahoo.fetch(symbol, range).await {
            Ok(chart) => db::upsert_prices(&pool, symbol, &chart)
                .await
                .map(|n| (chart.bars.len(), n)),
            Err(e) => Err(e),
        };
        match result {
            Ok((bars, written)) => {
                written_total += written;
                if !*has_prices {
                    println!("{symbol}: backfilled {bars} bars ({range})");
                }
            }
            Err(e) => {
                eprintln!("{symbol}: {e:#}");
                failed.push(symbol.as_str());
            }
        }
    }
    println!(
        "{}: {} symbols, {written_total} rows written, {} failed",
        market.as_str(),
        members.len(),
        failed.len()
    );
    if failed.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("failed: {}", failed.join(", "));
        Ok(ExitCode::FAILURE)
    }
}
