//! Yahoo Finance の chart API から日足の終値と出来高を取る。非公式 API のため、形が変わったら失敗させる。
use anyhow::{Context, Result, bail};
use jiff::{Timestamp, civil::Date};
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct DailyBar {
    /// 取引所の現地日付。
    pub date: Date,
    pub close: f64,
    pub volume: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chart {
    pub currency: String,
    pub bars: Vec<DailyBar>,
}

#[derive(Deserialize)]
struct Response {
    chart: ChartBody,
}
#[derive(Deserialize)]
struct ChartBody {
    result: Option<Vec<ChartResult>>,
    error: Option<serde_json::Value>,
}
#[derive(Deserialize)]
struct ChartResult {
    meta: Meta,
    #[serde(default)]
    timestamp: Vec<i64>,
    indicators: Indicators,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    currency: String,
    gmtoffset: i64,
    current_trading_period: Option<TradingPeriods>,
}
#[derive(Deserialize)]
struct TradingPeriods {
    regular: Period,
}
#[derive(Deserialize)]
struct Period {
    start: i64,
    end: i64,
}
#[derive(Deserialize)]
struct Indicators {
    quote: Vec<Quote>,
}
#[derive(Deserialize)]
struct Quote {
    #[serde(default)]
    close: Vec<Option<f64>>,
    #[serde(default)]
    volume: Vec<Option<i64>>,
}

fn local_date(ts: i64, gmtoffset: i64) -> Result<Date> {
    let offset = jiff::tz::Offset::from_seconds(i32::try_from(gmtoffset)?)?;
    Ok(offset.to_datetime(Timestamp::from_second(ts)?).date())
}

/// 応答を日足に直す。終値か出来高が無い日と、取引中でまだ確定していない当日分は除く。
pub fn parse(body: &str, now: Timestamp) -> Result<Chart> {
    let resp: Response = serde_json::from_str(body).context("unexpected chart response")?;
    if let Some(err) = resp.chart.error.filter(|e| !e.is_null()) {
        bail!("chart API error: {err}");
    }
    let result = resp
        .chart
        .result
        .and_then(|r| r.into_iter().next())
        .context("chart response has no result")?;
    let quote = result
        .indicators
        .quote
        .into_iter()
        .next()
        .context("chart response has no quote")?;
    let offset = result.meta.gmtoffset;
    let open_session = match result.meta.current_trading_period {
        Some(TradingPeriods { regular }) if now.as_second() < regular.end => {
            Some(local_date(regular.start, offset)?)
        }
        _ => None,
    };
    let mut bars = Vec::new();
    for (i, &ts) in result.timestamp.iter().enumerate() {
        let date = local_date(ts, offset)?;
        if Some(date) == open_session {
            continue;
        }
        let close = quote.close.get(i).copied().flatten();
        let volume = quote.volume.get(i).copied().flatten();
        if let (Some(close), Some(volume)) = (close, volume)
            && close.is_finite()
            && close > 0.0
            && volume >= 0
        {
            bars.push(DailyBar {
                date,
                close,
                volume,
            });
        }
    }
    // 同じ日が 2 本返ることがあるため、後のものを使う。
    bars.reverse();
    let mut seen = std::collections::HashSet::new();
    bars.retain(|b| seen.insert(b.date));
    bars.reverse();
    Ok(Chart {
        currency: result.meta.currency,
        bars,
    })
}

pub struct Client {
    http: reqwest::Client,
    base_url: String,
}

impl Client {
    pub fn new(http: reqwest::Client, base_url: String) -> Self {
        Self { http, base_url }
    }

    /// `range` は Yahoo の期間指定（`1mo`、`10y`、`max` など）。429 と 5xx は間を空けて 4 回まで試す。
    pub async fn fetch(&self, symbol: &str, range: &str) -> Result<Chart> {
        let url = format!("{}/v8/finance/chart/{symbol}", self.base_url);
        let mut wait = Duration::from_secs(2);
        for attempt in 1..=4 {
            let resp = self
                .http
                .get(&url)
                .query(&[("range", range), ("interval", "1d")])
                .send()
                .await;
            let retry = match resp {
                Ok(r) if r.status().is_success() => {
                    let body = r.text().await?;
                    return parse(&body, Timestamp::now()).with_context(|| symbol.to_string());
                }
                Ok(r) if r.status().as_u16() == 429 || r.status().is_server_error() => {
                    format!("HTTP {}", r.status())
                }
                Ok(r) => bail!("{symbol}: HTTP {}", r.status()),
                Err(e) => e.to_string(),
            };
            if attempt == 4 {
                bail!("{symbol}: {retry}");
            }
            eprintln!("{symbol}: {retry}, retrying in {}s", wait.as_secs());
            tokio::time::sleep(wait).await;
            wait *= 2;
        }
        unreachable!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 東証。1795046400 は 2026-11-19 09:00 JST、取引は 15:30 JST（1795069800）まで。
    const TSE: &str = r#"{"chart":{"result":[{
        "meta":{"currency":"JPY","gmtoffset":32400,
            "currentTradingPeriod":{"regular":{"start":1795046400,"end":1795069800}}},
        "timestamp":[1794873600,1794960000,1795046400],
        "indicators":{"quote":[{"close":[3414.0,null,3378.0],"volume":[15990400,17315400,19040800]}]}
    }],"error":null}}"#;

    fn d(s: &str) -> Date {
        s.parse().unwrap()
    }

    #[test]
    fn skips_missing_values_and_open_session() {
        let during = Timestamp::from_second(1795060000).unwrap();
        let chart = parse(TSE, during).unwrap();
        assert_eq!(chart.currency, "JPY");
        assert_eq!(
            chart.bars,
            [DailyBar {
                date: d("2026-11-17"),
                close: 3414.0,
                volume: 15990400
            }]
        );
        let after = Timestamp::from_second(1795069800).unwrap();
        let dates: Vec<_> = parse(TSE, after)
            .unwrap()
            .bars
            .iter()
            .map(|b| b.date)
            .collect();
        assert_eq!(dates, [d("2026-11-17"), d("2026-11-19")]);
    }

    #[test]
    fn uses_exchange_local_date() {
        // 1795012200 は 2026-11-18 09:30 EST（UTC では 14:30）。
        let body = r#"{"chart":{"result":[{"meta":{"currency":"USD","gmtoffset":-18000},
            "timestamp":[1795012200],
            "indicators":{"quote":[{"close":[352.45],"volume":[34090200]}]}}],"error":null}}"#;
        let chart = parse(body, Timestamp::from_second(1795100000).unwrap()).unwrap();
        assert_eq!(chart.bars[0].date, d("2026-11-18"));
    }

    #[test]
    fn keeps_last_bar_for_duplicate_dates() {
        let body = r#"{"chart":{"result":[{"meta":{"currency":"USD","gmtoffset":-18000},
            "timestamp":[1795012200,1795030000],
            "indicators":{"quote":[{"close":[1.0,2.0],"volume":[10,20]}]}}],"error":null}}"#;
        let chart = parse(body, Timestamp::from_second(1795100000).unwrap()).unwrap();
        assert_eq!(chart.bars.len(), 1);
        assert_eq!(chart.bars[0].close, 2.0);
    }

    #[test]
    fn reports_api_error() {
        let body = r#"{"chart":{"result":null,"error":{"code":"Not Found","description":"No data found"}}}"#;
        let err = parse(body, Timestamp::now()).unwrap_err().to_string();
        assert!(err.contains("Not Found"), "{err}");
        assert!(parse("<html>", Timestamp::now()).is_err());
    }
}
