# API reference

The merchant API reference is a static page, published at
<https://phala-network.github.io/phala-pay/>. It is built with
[Redoc](https://redocly.com/docs/redoc/) (`@redocly/cli`, pinned in `pnpm-lock.yaml`) from the
committed [`crates/topup/openapi.json`](../../crates/topup/openapi.json).

[`.github/workflows/api-reference.yml`](../../.github/workflows/api-reference.yml) publishes it on
every push to `main` that changes it, and CI's `sdk-js` job builds it on every pull request. Its
introduction, including the `Errors` section with one heading per code (the `doc_url` of every
error object), comes from `crates/topup/src/api/openapi.rs`.

To build it locally:

```sh
pnpm install --frozen-lockfile
pnpm run build   # dist/index.html
```
