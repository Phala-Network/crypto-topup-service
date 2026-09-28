from typing import Literal

AttestationResponseObject = Literal["attestation"]

ATTESTATION_RESPONSE_OBJECT_VALUES: set[AttestationResponseObject] = {
    "attestation",
}


def check_attestation_response_object(value: str) -> AttestationResponseObject:
    if value in ATTESTATION_RESPONSE_OBJECT_VALUES:
        return value
    raise TypeError(
        f"Unexpected value {value!r}. Expected one of {ATTESTATION_RESPONSE_OBJECT_VALUES!r}"
    )
