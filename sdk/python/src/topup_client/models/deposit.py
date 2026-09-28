from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.deposit_metadata import DepositMetadata
    from ..models.quote import Quote


T = TypeVar("T", bound="Deposit")


@_attrs_define
class Deposit:
    """A transfer to a quote's address at the route's confirmation: valued, screened, and credited,
    or rejected; `reversed` if its transaction left the chain before finality.

        Attributes:
            account_id (str): Your account identifier.
            address (str): Receiving forwarder address.
            amount_atomic (str): Token amount in base units, as a decimal string.
            amount_refunded_atomic (str): Refunded token amount in base units, as a decimal string: the sum of succeeded
                refunds.
            asset_contract (str): Token contract address.
            block_number (int): Number of the block the transfer is in; it changes if the transaction is re-included.
            chain_id (int): EVM chain identifier.
            created (int): Detection time, Unix seconds.
            currency (str): `usd`.
            from_address (str): Sender of the transfer.
            id (str): `dep_` and the hex of the deposit's deterministic UUID,
                `uuid_v5(DEPOSIT_NAMESPACE, "{chain_id}:{tx_hash}:{receipt_log_index}")`, where
                `receipt_log_index` is the transfer's position among its transaction's receipt logs.
            log_index (int): Block-wide log index of the transfer; it changes if the transaction is re-included.
            object_ (str): Always `deposit`.
            refunded (bool): Whether the deposit is fully refunded.
            status (str): `detected`, `confirmed`, `credited`, `swept`, `rejected`, or `reversed` (the transaction is
                not in the final chain: claw back a credit as for `deposit.refunded`). New values may be
                added.
            tx_hash (str): Transaction hash.
            amount (int | None | Unset): Credit in the currency's minor unit (cents), once valued.
            asset (None | str | Unset): Asset code; `null` for a token without a route.
            exchange_rate (None | str | Unset): USD per token, a decimal string with 8 places, once valued.
            metadata (DepositMetadata | Unset): Your key/value pairs ([metadata](https://docs.stripe.com/api/metadata)): a
                copy of the
                quote's when the deposit is recorded, independent of it afterwards; `{}` when none.
                Always sent; optional in the schema like the quote's.
            price_source (None | str | Unset): `quote` (the quoted price) or `spot`, once valued.
            quote (None | Quote | str | Unset):
            rejection_reason (None | str | Unset): Why the deposit was rejected: `unsupported_asset`, `below_minimum`,
                `out_of_bounds`,
                `out_of_range`, or `sanctioned`.
            valued_at (int | None | Unset): Valuation time, Unix seconds.
    """

    account_id: str
    address: str
    amount_atomic: str
    amount_refunded_atomic: str
    asset_contract: str
    block_number: int
    chain_id: int
    created: int
    currency: str
    from_address: str
    id: str
    log_index: int
    object_: str
    refunded: bool
    status: str
    tx_hash: str
    amount: int | None | Unset = UNSET
    asset: None | str | Unset = UNSET
    exchange_rate: None | str | Unset = UNSET
    metadata: DepositMetadata | Unset = UNSET
    price_source: None | str | Unset = UNSET
    quote: None | Quote | str | Unset = UNSET
    rejection_reason: None | str | Unset = UNSET
    valued_at: int | None | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.deposit_metadata import DepositMetadata  # noqa: PLC0415
        from ..models.quote import Quote  # noqa: PLC0415

        account_id = self.account_id

        address = self.address

        amount_atomic = self.amount_atomic

        amount_refunded_atomic = self.amount_refunded_atomic

        asset_contract = self.asset_contract

        block_number = self.block_number

        chain_id = self.chain_id

        created = self.created

        currency = self.currency

        from_address = self.from_address

        id = self.id

        log_index = self.log_index

        object_ = self.object_

        refunded = self.refunded

        status = self.status

        tx_hash = self.tx_hash

        amount: int | None | Unset
        if isinstance(self.amount, Unset):
            amount = UNSET
        else:
            amount = self.amount

        asset: None | str | Unset
        if isinstance(self.asset, Unset):
            asset = UNSET
        else:
            asset = self.asset

        exchange_rate: None | str | Unset
        if isinstance(self.exchange_rate, Unset):
            exchange_rate = UNSET
        else:
            exchange_rate = self.exchange_rate

        metadata: dict[str, Any] | Unset = UNSET
        if not isinstance(self.metadata, Unset):
            metadata = self.metadata.to_dict()

        price_source: None | str | Unset
        if isinstance(self.price_source, Unset):
            price_source = UNSET
        else:
            price_source = self.price_source

        quote: dict[str, Any] | None | str | Unset
        if isinstance(self.quote, Unset):
            quote = UNSET
        elif isinstance(self.quote, Quote):
            quote = self.quote.to_dict()
        else:
            quote = self.quote

        rejection_reason: None | str | Unset
        if isinstance(self.rejection_reason, Unset):
            rejection_reason = UNSET
        else:
            rejection_reason = self.rejection_reason

        valued_at: int | None | Unset
        if isinstance(self.valued_at, Unset):
            valued_at = UNSET
        else:
            valued_at = self.valued_at

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "account_id": account_id,
                "address": address,
                "amount_atomic": amount_atomic,
                "amount_refunded_atomic": amount_refunded_atomic,
                "asset_contract": asset_contract,
                "block_number": block_number,
                "chain_id": chain_id,
                "created": created,
                "currency": currency,
                "from_address": from_address,
                "id": id,
                "log_index": log_index,
                "object": object_,
                "refunded": refunded,
                "status": status,
                "tx_hash": tx_hash,
            }
        )
        if amount is not UNSET:
            field_dict["amount"] = amount
        if asset is not UNSET:
            field_dict["asset"] = asset
        if exchange_rate is not UNSET:
            field_dict["exchange_rate"] = exchange_rate
        if metadata is not UNSET:
            field_dict["metadata"] = metadata
        if price_source is not UNSET:
            field_dict["price_source"] = price_source
        if quote is not UNSET:
            field_dict["quote"] = quote
        if rejection_reason is not UNSET:
            field_dict["rejection_reason"] = rejection_reason
        if valued_at is not UNSET:
            field_dict["valued_at"] = valued_at

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.deposit_metadata import DepositMetadata  # noqa: PLC0415
        from ..models.quote import Quote  # noqa: PLC0415

        d = dict(src_dict)
        account_id = d.pop("account_id")

        address = d.pop("address")

        amount_atomic = d.pop("amount_atomic")

        amount_refunded_atomic = d.pop("amount_refunded_atomic")

        asset_contract = d.pop("asset_contract")

        block_number = d.pop("block_number")

        chain_id = d.pop("chain_id")

        created = d.pop("created")

        currency = d.pop("currency")

        from_address = d.pop("from_address")

        id = d.pop("id")

        log_index = d.pop("log_index")

        object_ = d.pop("object")

        refunded = d.pop("refunded")

        status = d.pop("status")

        tx_hash = d.pop("tx_hash")

        def _parse_amount(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        amount = _parse_amount(d.pop("amount", UNSET))

        def _parse_asset(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        asset = _parse_asset(d.pop("asset", UNSET))

        def _parse_exchange_rate(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        exchange_rate = _parse_exchange_rate(d.pop("exchange_rate", UNSET))

        _metadata = d.pop("metadata", UNSET)
        metadata: DepositMetadata | Unset
        if isinstance(_metadata, Unset):
            metadata = UNSET
        else:
            metadata = DepositMetadata.from_dict(_metadata)

        def _parse_price_source(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        price_source = _parse_price_source(d.pop("price_source", UNSET))

        def _parse_quote(data: object) -> None | Quote | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            try:
                if not isinstance(data, dict):
                    raise TypeError()
                componentsschemas_expandable_quote_type_1 = Quote.from_dict(data)

                return componentsschemas_expandable_quote_type_1
            except (TypeError, ValueError, AttributeError, KeyError):
                pass
            return cast(None | Quote | str | Unset, data)

        quote = _parse_quote(d.pop("quote", UNSET))

        def _parse_rejection_reason(data: object) -> None | str | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(None | str | Unset, data)

        rejection_reason = _parse_rejection_reason(d.pop("rejection_reason", UNSET))

        def _parse_valued_at(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        valued_at = _parse_valued_at(d.pop("valued_at", UNSET))

        deposit = cls(
            account_id=account_id,
            address=address,
            amount_atomic=amount_atomic,
            amount_refunded_atomic=amount_refunded_atomic,
            asset_contract=asset_contract,
            block_number=block_number,
            chain_id=chain_id,
            created=created,
            currency=currency,
            from_address=from_address,
            id=id,
            log_index=log_index,
            object_=object_,
            refunded=refunded,
            status=status,
            tx_hash=tx_hash,
            amount=amount,
            asset=asset,
            exchange_rate=exchange_rate,
            metadata=metadata,
            price_source=price_source,
            quote=quote,
            rejection_reason=rejection_reason,
            valued_at=valued_at,
        )

        deposit.additional_properties = d
        return deposit

    @property
    def additional_keys(self) -> list[str]:
        return list(self.additional_properties.keys())

    def __getitem__(self, key: str) -> Any:
        return self.additional_properties[key]

    def __setitem__(self, key: str, value: Any) -> None:
        self.additional_properties[key] = value

    def __delitem__(self, key: str) -> None:
        del self.additional_properties[key]

    def __contains__(self, key: str) -> bool:
        return key in self.additional_properties
