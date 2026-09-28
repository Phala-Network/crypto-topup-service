from typing import Literal

ClientQuoteObject = Literal["quote"]

CLIENT_QUOTE_OBJECT_VALUES: set[ClientQuoteObject] = {
    "quote",
}


def check_client_quote_object(value: str) -> ClientQuoteObject:
    if value in CLIENT_QUOTE_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {CLIENT_QUOTE_OBJECT_VALUES!r}")
