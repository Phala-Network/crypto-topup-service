from typing import Literal

ApiKeyObjectObject = Literal["api_key"]

API_KEY_OBJECT_OBJECT_VALUES: set[ApiKeyObjectObject] = {
    "api_key",
}


def check_api_key_object_object(value: str) -> ApiKeyObjectObject:
    if value in API_KEY_OBJECT_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {API_KEY_OBJECT_OBJECT_VALUES!r}")
