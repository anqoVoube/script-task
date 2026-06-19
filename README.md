# script-task

Fetch an [Upbit](https://www.upbit.com) notice and report whether a phrase
(default **"Market Support"**) appears in it.

The public notice page (`www.upbit.com/service_center/notice?id=N`) is an empty
React shell — the real title/body is loaded over XHR from the `api-manager` JSON
API. So this tool hits the JSON API first and falls back to the share page.

> Note: Upbit is geo-blocked / Cloudflare-protected in some regions. Run this
> where the URL is reachable (a server or VPN in an allowed region).

## Usage

```bash
cargo run --release                          # defaults: id 6303, "Market Support"
cargo run --release -- 6303 "Market Support" # explicit id + phrase
cargo run --release -- "https://api-manager.upbit.com/api/v1/announcements/6303?os=web"
```

## Exit codes

| code | meaning                                          |
|------|--------------------------------------------------|
| 0    | phrase found                                     |
| 2    | page reachable but phrase not found              |
| 1    | all requests failed (geo-block / network error)  |
