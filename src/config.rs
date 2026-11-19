//! 対象の指数。
use anyhow::{Result, bail};
use std::ops::RangeInclusive;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Market {
    Sp500,
    Nikkei225,
}

impl Market {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "sp500" => Ok(Market::Sp500),
            "nikkei225" => Ok(Market::Nikkei225),
            _ => bail!("MARKET must be sp500 or nikkei225: {s}"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Market::Sp500 => "sp500",
            Market::Nikkei225 => "nikkei225",
        }
    }

    /// 取得した構成銘柄の数がこの範囲外なら、取得元の形が変わったとみなして同期しない。
    /// S&P500 は複数の株式クラスを含むため 500 を少し超える。
    pub fn plausible_count(self) -> RangeInclusive<usize> {
        match self {
            Market::Sp500 => 490..=520,
            Market::Nikkei225 => 220..=230,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_market() {
        assert_eq!(Market::parse("sp500").unwrap(), Market::Sp500);
        assert_eq!(Market::parse("nikkei225").unwrap().as_str(), "nikkei225");
        assert!(Market::parse("SP500").is_err());
    }
}
