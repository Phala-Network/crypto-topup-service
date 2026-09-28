from typing import Literal

TreasuryChallengeObject = Literal["treasury_challenge"]

TREASURY_CHALLENGE_OBJECT_VALUES: set[TreasuryChallengeObject] = {
    "treasury_challenge",
}


def check_treasury_challenge_object(value: str) -> TreasuryChallengeObject:
    if value in TREASURY_CHALLENGE_OBJECT_VALUES:
        return value
    raise TypeError(
        f"Unexpected value {value!r}. Expected one of {TREASURY_CHALLENGE_OBJECT_VALUES!r}"
    )
