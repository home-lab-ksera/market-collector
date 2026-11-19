# market-collector

S&P500 と日経225 の全構成銘柄について、組み入れ・除外の履歴と日足（終値・出来高）を PostgreSQL に保存するバッチです。
home-lab-k8s の CronJob `market-collector-sp500` / `market-collector-nikkei225`（sandbox）として動きます。

## 動き

1 回の実行で 1 つの指数（`MARKET`）を扱います。

1. `migrations/` を適用する。
2. 構成銘柄の一覧を取る。
   - S&P500: 英語版 Wikipedia の [List of S&P 500 companies](https://en.wikipedia.org/wiki/List_of_S%26P_500_companies) の表（wikitext）。
   - 日経225: 日経の公開 CSV（`nikkei_stock_average_weight_jp.csv`、Shift_JIS）。月末時点のウエート一覧のため、入れ替えの反映が遅れることがある。
3. 件数が想定外（S&P500 は 490〜520、日経225 は 220〜230）なら、取得元の形が変わったとみなして何もせず失敗する。
4. `index_memberships` と突き合わせる。一覧に現れた銘柄は期間を開き（`added_on`）、消えた銘柄は閉じる（`removed_on`）。
   再び組み入れられたら新しい期間を作る。ティッカーの変更は「旧ティッカーの除外と新ティッカーの組み入れ」として記録される。
5. 有効な全銘柄の日足を Yahoo Finance から取り、`daily_prices` に upsert する。
   日足の無い銘柄は `BACKFILL_RANGE`（既定 `10y`）、それ以外は `LOOKBACK_RANGE`（既定 `1mo`）を取り直す。値が変わった行だけ更新する。
6. 取引中でまだ確定していない当日分と、終値か出来高が無い日は保存しない。
7. 1 銘柄でも価格が取れなければ、残りを処理したうえで終了コード 1 で終わる。

`added_on` / `removed_on` は変化を検知した日（JST）で、指数の正式な実施日ではありません。
初回の実行より前の組み入れ・除外は持ちません（初回の銘柄は初回の日付で組み入れとして記録されます）。
除外した銘柄の日足は消しません。

Yahoo Finance の chart API は非公式です。応答の形が変わると失敗します。
日経の CSV は日経の著作物です。取り込んだ一覧を再配布しないでください（このリポジトリのテストは架空のデータを使っています）。

## 設定

| 環境変数 | 内容 |
|---|---|
| `MARKET` | `sp500` か `nikkei225`（必須） |
| `LOOKBACK_RANGE` | 日足がある銘柄で毎回取り直す期間。既定 `1mo` |
| `BACKFILL_RANGE` | 日足の無い銘柄で取る期間。既定 `10y`（`max` で上場来） |
| `REQUEST_INTERVAL_MS` | Yahoo への要求の間隔。既定 `1000` |
| `CONSTITUENTS_URL` `YAHOO_BASE_URL` `USER_AGENT` | 取得先と User-Agent の上書き（テスト用） |
| `PGHOST` `PGUSER` `PGDATABASE` `PGPASSWORD` `PGSSLROOTCERT` | 共用 PostgreSQL への接続（`verify-full`） |
| `DATABASE_URL` | 指定すると `PG*` より優先する（ローカル用） |

## テーブル

```sql
index_memberships (
  id bigint, market text,            -- 'sp500' | 'nikkei225'
  symbol text, name text,            -- symbol は Yahoo のシンボル（BRK-B、7203.T）
  added_on date, removed_on date     -- removed_on が NULL なら現在の構成銘柄
)
daily_prices (
  symbol text, trade_date date,      -- 主キー。trade_date は取引所の現地日付
  currency text, close numeric(18,4), volume bigint, fetched_at timestamptz
)
```

ある日の構成銘柄の終値:

```sql
SELECT m.symbol, m.name, p.close, p.volume
FROM index_memberships m
JOIN daily_prices p ON p.symbol = m.symbol AND p.trade_date = DATE '2026-11-18'
WHERE m.market = 'sp500'
  AND m.added_on <= p.trade_date AND (m.removed_on IS NULL OR m.removed_on > p.trade_date);
```

## 開発

```bash
cargo test --locked
# DB を使うテスト（空の DB を用意する）
TEST_DATABASE_URL=postgres://postgres@127.0.0.1:5432/market_test cargo test --locked -- --ignored --test-threads=1
# ローカルで実行
DATABASE_URL=postgres://... MARKET=nikkei225 cargo run
```

`main` への push で GitHub Actions が検証し、Harbor の `sandbox/market-collector:latest` に公開します。
