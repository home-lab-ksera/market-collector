-- 指数の構成銘柄。取り込みのたびに公開リストと突き合わせ、組み入れ・除外を期間として残す。
-- added_on / removed_on は変化を検知した日（取引所の公表日・実施日ではない）。
CREATE TABLE index_memberships (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    market     text   NOT NULL CHECK (market IN ('sp500', 'nikkei225')),
    -- Yahoo Finance のシンボル（例: `BRK-B`、`7203.T`）。
    symbol     text   NOT NULL,
    name       text   NOT NULL,
    added_on   date   NOT NULL,
    removed_on date   CHECK (removed_on IS NULL OR removed_on >= added_on)
);

-- 同じ指数で、同時に有効な期間は 1 つだけ。
CREATE UNIQUE INDEX index_memberships_current ON index_memberships (market, symbol) WHERE removed_on IS NULL;
CREATE INDEX index_memberships_symbol ON index_memberships (symbol);

-- 銘柄ごとの日足。取引日は取引所の現地日付。除外後も消さない。
CREATE TABLE daily_prices (
    symbol     text           NOT NULL,
    trade_date date           NOT NULL,
    currency   text           NOT NULL,
    close      numeric(18, 4) NOT NULL CHECK (close > 0),
    volume     bigint         NOT NULL CHECK (volume >= 0),
    fetched_at timestamptz    NOT NULL DEFAULT now(),
    PRIMARY KEY (symbol, trade_date)
);
