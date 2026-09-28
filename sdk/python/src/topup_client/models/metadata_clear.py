from enum import StrEnum


class MetadataClear(StrEnum):
    VALUE_0 = ""

    def __str__(self) -> str:
        return str(self.value)
