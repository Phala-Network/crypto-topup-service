# USDT fee switch and blacklist

**Trigger:** on a route of Tether's USDT ([examples/phala-cloud-usdt.yaml](../../examples/phala-cloud-usdt.yaml)),
any of these:

- the fee check below finds a nonzero fee;
- Tether emits `Params`;
- a merchant's sweep of a USDT forwarder ends in `FlushFailed` with an empty `reason`;
- Tether blacklists a forwarder or a treasury (`AddedBlackList`, or a Tether notice).

The service raises no alert for any of these.

**Impact:** Tether's `TetherToken` keeps two controls that a plain ERC-20 lacks, and the service
cannot undo either one:

- **The fee switch.** `setParams` caps it at under 20 basis points and under 50 USDT per transfer,
  and it is zero today. While it is on, every transfer, a flush included, delivers less than it
  moves. `Flushed.amount` is what left the forwarder (`contracts/src/Forwarder.sol`), so the
  treasury receives less than the deposits it credited. Reconciliation compares each forwarder's
  balance with its deposits minus its `Flushed` amounts, never the treasury's net receipt, so it
  finds nothing.
- **The blacklist.** `transfer` checks only the sender, `isBlackListed[msg.sender]`, never the
  recipient. A blacklisted forwarder still receives payers' transfers, and the service credits them,
  since screening checks the payer on the sanctions oracle, not Tether's list. Its flush then fails
  with empty revert data, and the deposit stays in the forwarder, credited and not reversed. A
  blacklisted treasury does not fail the flush at all: the funds arrive where the treasury cannot
  move them.

No pause stops an on-chain transfer to an issued address, or a flush, which anyone can send. There
is no rescue path around the blacklist either. The factory and its forwarders have no owner, admin,
or rescue function, and a forwarder only ever pays its own treasury. Only Tether moves those funds:
`removeBlackList` (after which anyone can flush), or `destroyBlackFunds`, which burns the balance.
Tether can also `pause` the whole token, which makes every flush fail the same way until it
unpauses.

## Fee monitoring

Run this check daily on each chain with a USDT route, or as your own scheduled job alerting on any
nonzero result. `TOKEN` is the route's `asset.contract`:

```sh
cast call "$TOKEN" 'basisPointsRate()(uint256)' --rpc-url "$RPC_PROVIDER_A_URL"
cast call "$TOKEN" 'maximumFee()(uint256)' --rpc-url "$RPC_PROVIDER_A_URL"
cast logs --address "$TOKEN" 'Params(uint256,uint256)' --from-block "$LAST_CHECKED_BLOCK" --rpc-url "$RPC_PROVIDER_A_URL"
```

## Fee switch on

1. Stop new quotes and addresses, and further crediting, on every USDT route of the chain:

   ```sh
   admin POST "/v1/admin/routes/$ROUTE/pause" '{"scopes":["quotes","settlement"]}'
   ```

   `quotes` alone is not enough: existing addresses keep receiving payments, and only
   `settlement` holds those deposits `pending` instead of crediting them. A sanctioned or
   out-of-bounds deposit is still rejected.
2. Tell each merchant with USDT forwarders ([Incident communication](incident-communication.md)).
   Read their exposure from the route's `unflushed_balance_atomic` in the daily report
   (`admin GET /v1/admin/reports/daily`). While the fee is on:
   - each forwarder's sweep loses the fee, up to 50 USDT;
   - payers' transfers arrive short and stay `pending`;
   - sweeping is their choice, accepting the loss.
3. Do **not** follow [route retirement](route-retirement.md). It asks merchants to sweep every
   forwarder, and once the route is removed, its sweeps and reversals are no longer recorded.
   Keep the route loaded and paused.

Resume once both values read zero again: first `settlement`, which credits the held deposits at
what reached their forwarders, then `quotes`:

```sh
admin POST "/v1/admin/routes/$ROUTE/resume" '{"scopes":["settlement","quotes"]}'
```

If the fee stays on, retiring the route is a finance decision, taken with each merchant's
acceptance of the fee on its sweeps.

## Blacklisted forwarder

Confirm it (`$FORWARDER` is the failed target's forwarder):

```sh
cast call "$TOKEN" 'isBlackListed(address)(bool)' "$FORWARDER" --rpc-url "$RPC_PROVIDER_A_URL"
```

1. Hold further credits to the customer behind it while the merchant decides. The merchant finds
   the forwarder's quote or deposit address in `GET /v1/forwarders`:

   ```sh
   admin POST "/v1/admin/accounts/$ACCOUNT/customers/$CUSTOMER/pause" '{"scopes":["settlement"],"livemode":true}'
   ```

2. Tell the merchant that the forwarder's credited deposits are funds it does not hold and may
   never hold. They are listed by `GET /v1/deposits?quote=…` or `?deposit_address=…`. The service
   cannot reverse a credit for this, since the transfer is final. Taking the customer's credit back
   is the merchant's decision, in its own ledger. The balance stays in the route's
   `unflushed_balance_atomic`.
3. Resume the customer (`…/customers/$CUSTOMER/resume`, same body) once the merchant has decided.
   If Tether removes the forwarder from its blacklist, anyone can flush it as usual.

## Blacklisted treasury

Confirm it:

```sh
cast call "$TOKEN" 'isBlackListed(address)(bool)' "$TREASURY" --rpc-url "$RPC_PROVIDER_A_URL"
```

1. Pause crediting of the treasury, so new payments over it are held
   ([Treasury crediting pause](treasury-credit-pause.md)):

   ```sh
   admin POST "/v1/admin/accounts/$ACCOUNT/treasuries/$TREASURY_ID/pause" \
     '{"reason":"<ticket>: treasury blacklisted by Tether on USDT"}'
   ```

2. Tell the merchant to stop sweeping USDT forwarders over it, since each sweep lands funds it
   cannot move, and to move to a new treasury ([Treasury change](treasury-change.md)). USDT already
   swept there is frozen until Tether acts.

## Done when

- **Fee:** both fee values read zero and the route's scopes are resumed, or finance has decided to
  retire it.
- **Blacklist:** the merchant has decided on the affected credits, the paused customer or treasury
  is resumed or replaced, and new USDT payments reach a treasury that can move them.
