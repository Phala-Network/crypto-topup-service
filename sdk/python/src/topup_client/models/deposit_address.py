from __future__ import annotations

from collections.abc import Mapping
from typing import Any, TypeVar, BinaryIO, TextIO, TYPE_CHECKING, Generator

from attrs import define as _attrs_define
from attrs import field as _attrs_field

from ..types import UNSET, Unset

from ..types import UNSET, Unset
from typing import cast

if TYPE_CHECKING:
    from ..models.deposit_address_metadata import DepositAddressMetadata


T = TypeVar("T", bound="DepositAddress")


@_attrs_define
class DepositAddress:
    """A customer's persistent deposit address for one chain and asset, like a bank-transfer virtual
    account: any amount sent to it is credited to the customer at the market (spot) price when it
    arrives. Rotation retires it and issues a new one; a retired address is still credited.

        Attributes:
            address (str): The forwarder address to pay.
            asset (str): Asset code.
            chain_id (int): EVM chain identifier.
            client_reference_id (str): Your identifier of the customer.
            created (int): Creation time, Unix seconds.
            id (str): `da_` id.
            livemode (bool): Whether the address is in live mode.
            metadata (DepositAddressMetadata): Your key/value pairs ([metadata](https://docs.stripe.com/api/metadata)); `{}`
                when none.
                Each deposit to the address starts with a copy.
            object_ (str): Always `deposit_address`.
            payment_uri (str): EIP-681 ERC-20 transfer URI carrying the token, chain, and address, and no amount: the
                payer chooses it.
            salt (str): CREATE2 salt, 32 bytes of hex.
            status (str): `active`, or `retired` by a rotation; payments to either are credited.
            treasury (str): The treasury the forwarder pays, fixed when the address was issued.
            version (int): The address's version among the customer's addresses for this chain and asset, from 1.
                The salt is `keccak256(abi.encode(account, livemode, client_reference_id,
                "deposit_address", chain_id, asset, version))`, with the types `(string, bool, string,
                string, uint256, string, uint256)` and `account` your `acct_` id; the address is the
                factory's `CREATE2` over the treasury and that salt.
            retired_at (int | None | Unset): Retirement time, Unix seconds; `null` while active.
    """

    address: str
    asset: str
    chain_id: int
    client_reference_id: str
    created: int
    id: str
    livemode: bool
    metadata: DepositAddressMetadata
    object_: str
    payment_uri: str
    salt: str
    status: str
    treasury: str
    version: int
    retired_at: int | None | Unset = UNSET
    additional_properties: dict[str, Any] = _attrs_field(init=False, factory=dict)

    def to_dict(self) -> dict[str, Any]:
        from ..models.deposit_address_metadata import DepositAddressMetadata  # noqa: PLC0415

        address = self.address

        asset = self.asset

        chain_id = self.chain_id

        client_reference_id = self.client_reference_id

        created = self.created

        id = self.id

        livemode = self.livemode

        metadata = self.metadata.to_dict()

        object_ = self.object_

        payment_uri = self.payment_uri

        salt = self.salt

        status = self.status

        treasury = self.treasury

        version = self.version

        retired_at: int | None | Unset
        if isinstance(self.retired_at, Unset):
            retired_at = UNSET
        else:
            retired_at = self.retired_at

        field_dict: dict[str, Any] = {}
        field_dict.update(self.additional_properties)
        field_dict.update(
            {
                "address": address,
                "asset": asset,
                "chain_id": chain_id,
                "client_reference_id": client_reference_id,
                "created": created,
                "id": id,
                "livemode": livemode,
                "metadata": metadata,
                "object": object_,
                "payment_uri": payment_uri,
                "salt": salt,
                "status": status,
                "treasury": treasury,
                "version": version,
            }
        )
        if retired_at is not UNSET:
            field_dict["retired_at"] = retired_at

        return field_dict

    @classmethod
    def from_dict(cls: type[T], src_dict: Mapping[str, Any]) -> T:
        from ..models.deposit_address_metadata import DepositAddressMetadata  # noqa: PLC0415

        d = dict(src_dict)
        address = d.pop("address")

        asset = d.pop("asset")

        chain_id = d.pop("chain_id")

        client_reference_id = d.pop("client_reference_id")

        created = d.pop("created")

        id = d.pop("id")

        livemode = d.pop("livemode")

        metadata = DepositAddressMetadata.from_dict(d.pop("metadata"))

        object_ = d.pop("object")

        payment_uri = d.pop("payment_uri")

        salt = d.pop("salt")

        status = d.pop("status")

        treasury = d.pop("treasury")

        version = d.pop("version")

        def _parse_retired_at(data: object) -> int | None | Unset:
            if data is None:
                return data
            if isinstance(data, Unset):
                return data
            return cast(int | None | Unset, data)

        retired_at = _parse_retired_at(d.pop("retired_at", UNSET))

        deposit_address = cls(
            address=address,
            asset=asset,
            chain_id=chain_id,
            client_reference_id=client_reference_id,
            created=created,
            id=id,
            livemode=livemode,
            metadata=metadata,
            object_=object_,
            payment_uri=payment_uri,
            salt=salt,
            status=status,
            treasury=treasury,
            version=version,
            retired_at=retired_at,
        )

        deposit_address.additional_properties = d
        return deposit_address

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
