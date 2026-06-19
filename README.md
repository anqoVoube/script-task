# script-task

Fetch an [Upbit](https://www.upbit.com) notice and **print its title** — in
English by default.

The public notice page (`www.upbit.com/service_center/notice?id=N`) is an empty
React shell; the real title/body comes from the `api-manager` JSON API. That API
serves Korean by default, and `?os=web&language=en` returns English.

The title is printed to **stdout** (one clean line, script-friendly). The URL,
status, category, and any `--match` results go to **stderr**.

> Note: Upbit is geo-blocked / Cloudflare-protected in some regions. Run this
> where the URL is reachable (a server or VPN in an allowed region).

## Usage

```bash
cargo run --release                       # id 6303, English title
cargo run --release -- 6303               # explicit id
cargo run --release -- 6303 --lang ko     # Korean title
cargo run --release -- 6303 --body        # also print the body text
cargo run --release -- 6303 --match "Market Support"   # also report phrase hits
cargo run --release -- 6303 --probe       # try every endpoint variant
```

Just the title, nothing else:

```bash
cargo run --release --quiet -- 6303 2>/dev/null
# -> Market Support for Re(RE) (KRW, BTC, USDT Market)
```

## Flags

| flag             | effect                                             |
|------------------|----------------------------------------------------|
| `--lang en\|ko`  | language (default `en`)                            |
| `--body`         | also print the notice body as plain text (stdout)  |
| `--match PHRASE` | also report case-insensitive occurrences (stderr)  |
| `--probe`        | try all endpoint variants, print a diagnostic table|
| `--raw`          | dump the raw JSON to stderr                         |

## Exit codes

| code | meaning                                          |
|------|--------------------------------------------------|
| 0    | title printed                                    |
| 2    | reachable but no `title` field in the response   |
| 1    | all requests failed (geo-block / network error)  |
