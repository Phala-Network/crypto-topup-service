# API reference

The merchant API reference, a static page built with [Redoc](https://redocly.com/docs/redoc/)
(`@redocly/cli`, pinned in `pnpm-lock.yaml`) from the committed
[`crates/topup/openapi.json`](../../crates/topup/openapi.json), and published to
<https://phala-network.github.io/phala-pay/> by
[`.github/workflows/api-reference.yml`](../../.github/workflows/api-reference.yml) on every push
to `main` that changes it; CI's `sdk-js` job builds it on every pull request. Its introduction,
including the `Errors` section with one heading per code (the `doc_url` of every error object),
comes from `crates/topup/src/api/openapi.rs`.

```sh
pnpm install --frozen-lockfile
pnpm run build   # dist/index.html
```
