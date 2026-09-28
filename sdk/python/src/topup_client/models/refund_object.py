from typing import Literal

RefundObject = Literal["refund"]

REFUND_OBJECT_VALUES: set[RefundObject] = {
    "refund",
}


def check_refund_object(value: str) -> RefundObject:
    if value in REFUND_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {REFUND_OBJECT_VALUES!r}")
