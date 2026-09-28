from typing import Literal

ApiKeyListObject = Literal["list"]

API_KEY_LIST_OBJECT_VALUES: set[ApiKeyListObject] = {
    "list",
}


def check_api_key_list_object(value: str) -> ApiKeyListObject:
    if value in API_KEY_LIST_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {API_KEY_LIST_OBJECT_VALUES!r}")
