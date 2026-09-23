# Local exercise setup

Date: 2026-09-22. This is the shared environment for the exercises in this directory. Nothing here
touches a live environment; every container, process, and database is task-scoped and removed
afterward.

PostgreSQL 18 and Anvil (the exercises were recorded on PostgreSQL 16; CI now runs their integration
tests on 18):

```sh
export PATH="$HOME/.cargo/bin:$HOME/.foundry/bin:$PATH"
docker run -d --rm --name wp-d5-exercise-pg -e POSTGRES_PASSWORD=postgres \
  -p 127.0.0.1:55436:5432 \
  postgres:18.6-trixie@sha256:86c951e05bf56c93d95d397747fb8820ac76cc3bedb78f43abd83eedbe3666ae
mkdir -p /tmp/wp-d5-ex
anvil --port 8547 --chain-id 31337 --silent > /tmp/wp-d5-ex/anvil.log 2>&1 &
echo "$!" > /tmp/wp-d5-ex/anvil.pid
cargo build --locked -q -p topup
```

Integration tests create and drop their own databases and application roles when both URLs are
set; without them they print a skip message instead of running:

```sh
export SQLX_OFFLINE=true
export MIGRATE_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55436/postgres
export DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55436/postgres
```

For the CLI exercises, deploy a mock token and a real `ForwarderFactory` on Anvil with Anvil's
public development key #0 as admin and development account #1 as treasury:

```sh
export ANVIL_KEY=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
cd contracts
forge create test/mocks/MockTokens.sol:MockERC20 \
  --rpc-url http://127.0.0.1:8547 --private-key "$ANVIL_KEY" --broadcast
forge create src/ForwarderFactory.sol:ForwarderFactory \
  --rpc-url http://127.0.0.1:8547 --private-key "$ANVIL_KEY" --broadcast \
  --constructor-args 0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266 0x70997970C51812dc3A010C7d01b50e0d17dc79C8
cast rpc anvil_mine 70 --rpc-url http://127.0.0.1:8547
cd ..
```

Observed: token `0x5FbDB2315678afecb367f032d93F642f64180aa3`, factory
`0xe7f1725E7734CE288F8367e1Bb143E90bb3F0512`, implementation
`0xCafac3dD18aC6c6e92c921884f9E4176737C052c`.

The route file is the committed Sepolia template with chain id `31337`, the deployed
factory/implementation/treasury/token, an unreachable settlement URL, and route name
`local-anvil-pha-usd`. The first run added `rate_lock.max_creations_per_minute: 10` by hand; since
#72 the template carries it:

```sh
topup route validate /tmp/wp-d5-ex/route.yaml
```

Observed:

```text
route file `/tmp/wp-d5-ex/route.yaml` is valid at schema level; on-chain deployment and Safe control were not checked
```

Each CLI exercise uses its own database, migrated by `topup migrate` as the owner and read through a
login role in `topup_app`:

```sh
docker exec wp-d5-exercise-pg createdb -U postgres wp_d5_recon
MIGRATE_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55436/wp_d5_recon topup migrate
docker exec wp-d5-exercise-pg psql -U postgres -d wp_d5_recon \
  -c "CREATE ROLE wp_d5_app LOGIN PASSWORD 'wp_d5_app' IN ROLE topup_app;"
export DATABASE_URL=postgres://wp_d5_app:wp_d5_app@127.0.0.1:55436/wp_d5_recon
export TOPUP_RPC_PROVIDER_A_URL=http://127.0.0.1:8547
export TOPUP_RPC_PROVIDER_B_URL=http://localhost:8547
```

## Runbook SQL

Every `psql` statement in the runbooks (heredoc bodies and `<<<` forms, 31 in total) was executed
against `wp_d5_recon2`, re-created after merging #70 and #72, with `-v ON_ERROR_STOP=1` and test
values for each `--set` variable: the 30 `$DATABASE_URL` statements as `wp_d5_app`, and the owner
`lock_exposure` repair as the owner with its `COMMIT` replaced by `ROLLBACK`. All 31 succeeded,
which checks their syntax, column names, and grants. The first pass also found that psql does not
interpolate `--set` variables inside `-c` strings, and psql before 15 prints only the last result of
a multi-statement `-c`; the runbooks therefore feed statements on stdin with `<<<`.

Teardown:

```sh
kill "$(cat /tmp/wp-d5-ex/anvil.pid)"
docker stop wp-d5-exercise-pg
rm -rf /tmp/wp-d5-ex
```
