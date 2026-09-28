from typing import Literal

QuoteListObject = Literal["list"]

QUOTE_LIST_OBJECT_VALUES: set[QuoteListObject] = {
    "list",
}


def check_quote_list_object(value: str) -> QuoteListObject:
    if value in QUOTE_LIST_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {QUOTE_LIST_OBJECT_VALUES!r}")
