//! 指数の現在の構成銘柄を取る。
//! S&P500 は英語版 Wikipedia の一覧（wikitext）、日経225 は日経の公開 CSV（Shift_JIS）を使う。
use crate::config::Market;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;

pub const SP500_URL: &str =
    "https://en.wikipedia.org/w/index.php?title=List_of_S%26P_500_companies&action=raw";
pub const NIKKEI225_URL: &str =
    "https://indexes.nikkei.co.jp/nkave/archives/file/nikkei_stock_average_weight_jp.csv";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constituent {
    /// Yahoo Finance のシンボル。
    pub symbol: String,
    pub name: String,
}

pub async fn fetch(http: &reqwest::Client, market: Market, url: &str) -> Result<Vec<Constituent>> {
    let resp = http.get(url).send().await?.error_for_status()?;
    let body = resp.bytes().await?;
    let list = match market {
        Market::Sp500 => parse_sp500(std::str::from_utf8(&body)?)?,
        Market::Nikkei225 => parse_nikkei225(&body)?,
    };
    check_count(market, &list)?;
    Ok(list)
}

pub fn check_count(market: Market, list: &[Constituent]) -> Result<()> {
    let range = market.plausible_count();
    if !range.contains(&list.len()) {
        bail!(
            "{}: got {} constituents, expected {}..={}; the source format may have changed",
            market.as_str(),
            list.len(),
            range.start(),
            range.end()
        );
    }
    Ok(())
}

fn dedup(list: Vec<Constituent>) -> Result<Vec<Constituent>> {
    let mut map = BTreeMap::new();
    for c in list {
        if map.insert(c.symbol.clone(), c.clone()).is_some() {
            bail!("duplicate constituent: {}", c.symbol);
        }
    }
    Ok(map.into_values().collect())
}

/// `{{NyseSymbol|BRK.B}}` のようなテンプレートからティッカーを取り出す。
fn symbol_template(cell: &str) -> Option<&str> {
    let start = cell.find("{{")? + 2;
    let inner = &cell[start..start + cell[start..].find("}}")?];
    let (kind, ticker) = inner.split_once('|')?;
    kind.trim().ends_with("Symbol").then(|| ticker.trim())
}

/// `[[Alphabet Inc.|Alphabet Inc. (Class A)]]` → 表示名。リンクでなければそのまま。
fn link_text(cell: &str) -> String {
    let cell = cell.trim();
    match cell.strip_prefix("[[").and_then(|s| s.split_once("]]")) {
        Some((link, rest)) => {
            let text = link.rsplit_once('|').map_or(link, |(_, t)| t);
            format!("{text}{rest}").trim().to_string()
        }
        None => cell.to_string(),
    }
}

/// 一覧の表（`id="constituents"`）の各行の 1 列目がティッカー、2 列目が社名。
pub fn parse_sp500(wikitext: &str) -> Result<Vec<Constituent>> {
    let start = wikitext
        .find("id=\"constituents\"")
        .context("constituents table not found")?;
    let table = &wikitext[start..];
    let table = &table[..table
        .find("\n|}")
        .context("constituents table is not closed")?];
    let mut out = Vec::new();
    for row in table.split("\n|-").skip(1) {
        // 行は `|| セル` を 1 行ずつ、または `| a || b` の形で書かれる。
        let cells: Vec<&str> = row
            .lines()
            .filter(|l| l.starts_with('|'))
            .flat_map(|l| l.trim_start_matches('|').split("||"))
            .map(|c| c.split("<!--").next().unwrap_or("").trim())
            .collect();
        let Some(ticker) = cells.first().and_then(|c| symbol_template(c)) else {
            continue;
        };
        if ticker.is_empty()
            || !ticker
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.')
        {
            bail!("unexpected ticker: {ticker}");
        }
        out.push(Constituent {
            // Yahoo では株式クラスを `-` でつなぐ（BRK.B → BRK-B）。
            symbol: ticker.to_ascii_uppercase().replace('.', "-"),
            name: cells.get(1).map(|c| link_text(c)).unwrap_or_default(),
        });
    }
    dedup(out)
}

/// `"日付","コード","社名",...` の行から、4 桁の証券コードと社名を取る。末尾の注記行は読み飛ばす。
pub fn parse_nikkei225(body: &[u8]) -> Result<Vec<Constituent>> {
    let (text, _, had_errors) = encoding_rs::SHIFT_JIS.decode(body);
    if had_errors {
        bail!("nikkei225 csv is not valid Shift_JIS");
    }
    let mut lines = text.lines();
    let header = lines.next().context("empty csv")?;
    if !header
        .trim_start_matches('\u{feff}')
        .starts_with("日付,コード,社名")
    {
        bail!("unexpected csv header: {header}");
    }
    let mut out = Vec::new();
    for line in lines {
        let fields: Vec<&str> = line
            .split(',')
            .map(|f| f.trim().trim_matches('"'))
            .collect();
        let [_, code, name, ..] = fields.as_slice() else {
            continue;
        };
        if code.len() == 4
            && code
                .chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
        {
            out.push(Constituent {
                symbol: format!("{code}.T"),
                name: name.to_string(),
            });
        }
    }
    dedup(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 実際の一覧と同じ書き方の、架空の 4 行。
    const WIKI: &str = r#"intro {{NyseSymbol|XXX}} outside table
{| class="wikitable sortable" id="constituents"
|-
![[Ticker symbol|Symbol]]
! Security !! GICS Sector
|-
|| {{NyseSymbol|AAA}}
|| [[Alpha Corp]]
|| Industrials
|-
|| {{NyseSymbol|BBB.B}} <!-- DO NOT CHANGE -->
|| [[Beta Holdings|Beta Holdings (Class B)]]
|| Financials
|-
| {{NasdaqSymbol|ccc}} || Gamma Inc. || Information Technology
|-
|| no symbol row
|}
{| class="wikitable" id="changes"
|-
|| {{NyseSymbol|ZZZ}}
|}"#;

    #[test]
    fn parses_sp500_table_only() {
        let got = parse_sp500(WIKI).unwrap();
        let got: Vec<_> = got
            .iter()
            .map(|c| (c.symbol.as_str(), c.name.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("AAA", "Alpha Corp"),
                ("BBB-B", "Beta Holdings (Class B)"),
                ("CCC", "Gamma Inc."),
            ]
        );
        assert!(parse_sp500("no table").is_err());
    }

    #[test]
    fn parses_nikkei225_shift_jis() {
        let csv = "日付,コード,社名,業種,セクター,ウエート\r\n\
            \"2026/10/30\",\"1001\",\"架空水産\",\"水産\",\"消費\",\"0.1000%\"\r\n\
            \"2026/10/30\",\"285A\",\"架空テック\",\"電気機器\",\"技術\",\"0.2000%\"\r\n\
            \"本資料は注記です。\"\r\n";
        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode(csv);
        let got = parse_nikkei225(&sjis).unwrap();
        assert_eq!(
            got,
            [
                Constituent {
                    symbol: "1001.T".into(),
                    name: "架空水産".into()
                },
                Constituent {
                    symbol: "285A.T".into(),
                    name: "架空テック".into()
                },
            ]
        );
        let (bad, _, _) = encoding_rs::SHIFT_JIS.encode("code,name\r\n");
        assert!(parse_nikkei225(&bad).is_err());
    }

    #[test]
    fn rejects_implausible_counts_and_duplicates() {
        let one = vec![Constituent {
            symbol: "AAA".into(),
            name: "A".into(),
        }];
        assert!(check_count(Market::Sp500, &one).is_err());
        let many: Vec<_> = (0..225)
            .map(|i| Constituent {
                symbol: format!("{i:04}.T"),
                name: String::new(),
            })
            .collect();
        assert!(check_count(Market::Nikkei225, &many).is_ok());
        assert!(dedup([one.clone(), one].concat()).is_err());
    }
}
