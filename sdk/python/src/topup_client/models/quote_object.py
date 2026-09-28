from typing import Literal

QuoteObject = Literal["quote"]

QUOTE_OBJECT_VALUES: set[QuoteObject] = {
    "quote",
}


def check_quote_object(value: str) -> QuoteObject:
    if value in QUOTE_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {QUOTE_OBJECT_VALUES!r}")
