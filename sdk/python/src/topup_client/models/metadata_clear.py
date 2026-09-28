from typing import Literal

MetadataClear = Literal[""]

METADATA_CLEAR_VALUES: set[MetadataClear] = {
    "",
}


def check_metadata_clear(value: str) -> MetadataClear:
    if value in METADATA_CLEAR_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {METADATA_CLEAR_VALUES!r}")
