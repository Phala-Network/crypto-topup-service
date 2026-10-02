from typing import Literal

PaymentSettingsObjectObject = Literal["payment_settings"]

PAYMENT_SETTINGS_OBJECT_OBJECT_VALUES: set[PaymentSettingsObjectObject] = {
    "payment_settings",
}


def check_payment_settings_object_object(value: str) -> PaymentSettingsObjectObject:
    if value in PAYMENT_SETTINGS_OBJECT_OBJECT_VALUES:
        return value
    raise TypeError(
        f"Unexpected value {value!r}. Expected one of {PAYMENT_SETTINGS_OBJECT_OBJECT_VALUES!r}"
    )
