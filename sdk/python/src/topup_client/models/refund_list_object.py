from typing import Literal

RefundListObject = Literal["list"]

REFUND_LIST_OBJECT_VALUES: set[RefundListObject] = {
    "list",
}


def check_refund_list_object(value: str) -> RefundListObject:
    if value in REFUND_LIST_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {REFUND_LIST_OBJECT_VALUES!r}")
