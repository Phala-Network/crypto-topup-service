from typing import Literal

ConfigObject = Literal["config"]

CONFIG_OBJECT_VALUES: set[ConfigObject] = {
    "config",
}


def check_config_object(value: str) -> ConfigObject:
    if value in CONFIG_OBJECT_VALUES:
        return value
    raise TypeError(f"Unexpected value {value!r}. Expected one of {CONFIG_OBJECT_VALUES!r}")
