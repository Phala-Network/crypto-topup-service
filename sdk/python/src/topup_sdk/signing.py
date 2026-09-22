"""RFC 9421 HTTP Message Signatures with ed25519, in the service's exact profile.

The profile covers `@method`, `@target-uri`, and `content-digest` (SHA-256), plus
`idempotency-key` whenever that header is present. Parameters are `created` (Unix seconds,
accepted within five minutes) and `keyid`, with an optional `alg="ed25519"` and a random
`nonce`. Every request is signed afresh, and the service accepts each signature once. Because
ed25519 is deterministic, an identical request signed twice in the same second would otherwise
repeat its signature; the nonce keeps every signature unique while `created` stays the clock.
"""

from __future__ import annotations

import base64
import hashlib
import secrets
import time
from collections.abc import Callable, Generator, Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import cast

import http_sf
import httpx
from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

from .errors import SignatureError

COVERED_COMPONENTS = ("@method", "@target-uri", "content-digest")
IDEMPOTENCY_COMPONENT = "idempotency-key"
MAX_CLOCK_SKEW_SECONDS = 300
SIGNATURE_LABEL = "sig1"


def content_digest(body: bytes) -> str:
    """Returns the RFC 9530 `Content-Digest` value for `body`."""
    digest = base64.b64encode(hashlib.sha256(body).digest()).decode("ascii")
    return f"sha-256=:{digest}:"


def signature_params(
    keyid: str,
    created: int,
    *,
    cover_idempotency_key: bool,
    include_alg: bool = True,
    nonce: str | None = None,
) -> str:
    """Serializes the `@signature-params` inner list as an RFC 8941 structured field."""
    components = [*COVERED_COMPONENTS]
    if cover_idempotency_key:
        components.append(IDEMPOTENCY_COMPONENT)
    parameters: dict[str, object] = {"created": created, "keyid": keyid}
    if include_alg:
        parameters["alg"] = "ed25519"
    if nonce is not None:
        parameters["nonce"] = nonce
    try:
        return _sf_serialize([([(component, {}) for component in components], parameters)])
    except ValueError as error:
        raise ValueError("keyid and nonce must be printable ASCII") from error


def new_nonce() -> str:
    """Returns 128 random bits as URL-safe base64, a valid structured-field string."""
    return secrets.token_urlsafe(16)


def signature_base(
    method: str,
    target_uri: str,
    digest: str,
    idempotency_key: str | None,
    params: str,
) -> bytes:
    """Builds the exact RFC 9421 signature base for this profile."""
    lines = [
        f'"@method": {method.upper()}',
        f'"@target-uri": {target_uri}',
        f'"content-digest": {digest}',
    ]
    if idempotency_key is not None:
        lines.append(f'"{IDEMPOTENCY_COMPONENT}": {idempotency_key}')
    lines.append(f'"@signature-params": {params}')
    return "\n".join(lines).encode("ascii")


class RequestSigner:
    """Signs requests with a product's ed25519 key."""

    def __init__(
        self,
        keyid: str,
        private_key: Ed25519PrivateKey,
        *,
        include_alg: bool = True,
        include_nonce: bool = True,
        clock: Callable[[], float] = time.time,
    ) -> None:
        if not keyid:
            raise ValueError("keyid must not be empty")
        self.keyid = keyid
        self._private_key = private_key
        self._include_alg = include_alg
        self._include_nonce = include_nonce
        self._clock = clock

    @classmethod
    def from_seed(
        cls,
        keyid: str,
        seed: bytes,
        *,
        include_alg: bool = True,
        include_nonce: bool = True,
        clock: Callable[[], float] = time.time,
    ) -> RequestSigner:
        """Creates a signer from a raw 32-byte ed25519 seed."""
        if len(seed) != 32:
            raise ValueError("ed25519 seed must contain exactly 32 bytes")
        private_key = Ed25519PrivateKey.from_private_bytes(seed)
        return cls(
            keyid,
            private_key,
            include_alg=include_alg,
            include_nonce=include_nonce,
            clock=clock,
        )

    @classmethod
    def from_seed_file(cls, keyid: str, path: str | Path) -> RequestSigner:
        """Loads a seed file holding 64 hexadecimal characters."""
        return cls.from_seed(keyid, bytes.fromhex(Path(path).read_text(encoding="ascii").strip()))

    def public_key_base64(self) -> str:
        """Returns the raw public key in the standard-base64 form the service stores."""
        raw = self._private_key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
        return base64.b64encode(raw).decode("ascii")

    def sign(
        self,
        method: str,
        target_uri: str,
        body: bytes,
        *,
        idempotency_key: str | None = None,
        created: int | None = None,
        nonce: str | None = None,
    ) -> dict[str, str]:
        """Returns the `Content-Digest`, `Signature-Input`, and `Signature` headers.

        `target_uri` is the absolute URI exactly as the verifier reconstructs it:
        `scheme://host[:port]/path?query`, using the `Host` header the request carries.
        `idempotency_key` is the raw `Idempotency-Key` header value when one is sent.
        `created` defaults to the clock and `nonce` to a fresh random value; pass them only to
        reproduce fixed test vectors.
        """
        digest = content_digest(body)
        if created is None:
            created = int(self._clock())
        if nonce is None and self._include_nonce:
            nonce = new_nonce()
        params = signature_params(
            self.keyid,
            created,
            cover_idempotency_key=idempotency_key is not None,
            include_alg=self._include_alg,
            nonce=nonce,
        )
        base = signature_base(method, target_uri, digest, idempotency_key, params)
        signature = self._private_key.sign(base)
        return {
            "content-digest": digest,
            "signature-input": f"{SIGNATURE_LABEL}={params}",
            "signature": _sf_serialize({SIGNATURE_LABEL: (signature, {})}),
        }


class SigningAuth(httpx.Auth):
    """httpx authentication flow that signs every request, including each retry."""

    requires_request_body = True

    def __init__(self, signer: RequestSigner) -> None:
        self._signer = signer

    def auth_flow(self, request: httpx.Request) -> Generator[httpx.Request, httpx.Response, None]:
        request.headers.update(
            self._signer.sign(
                request.method,
                target_uri(request),
                request.content,
                idempotency_key=request.headers.get("idempotency-key"),
            )
        )
        yield request


def target_uri(request: httpx.Request) -> str:
    """Reconstructs `@target-uri` from the scheme, `Host` header, and raw path of a request."""
    host = request.headers.get("host") or request.url.netloc.decode("ascii")
    return f"{request.url.scheme}://{host}{request.url.raw_path.decode('ascii')}"


@dataclass(frozen=True)
class VerifiedRequest:
    """A request whose signature matched the pinned key."""

    keyid: str
    created: int
    idempotency_key: str | None


def load_public_key(encoded: str) -> Ed25519PublicKey:
    """Parses a raw ed25519 public key given as 64 hexadecimal characters or standard base64."""
    value = encoded.strip().removeprefix("0x")
    raw = bytes.fromhex(value) if len(value) == 64 else base64.b64decode(value, validate=True)
    if len(raw) != 32:
        raise ValueError("ed25519 public key must contain exactly 32 bytes")
    return Ed25519PublicKey.from_public_bytes(raw)


def verify_request(
    *,
    method: str,
    target_uri: str,
    headers: Mapping[str, str],
    body: bytes,
    public_key: Ed25519PublicKey,
    keyid: str,
    require_idempotency_key: bool,
    now: int | None = None,
) -> VerifiedRequest:
    """Verifies an inbound request signed in this profile, such as a settlement request.

    `target_uri` must be the URI the sender addressed, derived from the receiver's own configured
    public URL rather than from untrusted `Host` or forwarding headers. `keyid` and `public_key`
    are pinned together. Raises `SignatureError` without detail on any mismatch.
    """
    lowered = {name.lower(): value.strip() for name, value in headers.items()}
    digest = lowered.get("content-digest")
    if digest is None or digest != content_digest(body):
        raise SignatureError("content digest mismatch")
    idempotency_key = lowered.get(IDEMPOTENCY_COMPONENT)
    if require_idempotency_key and idempotency_key is None:
        raise SignatureError("idempotency key missing")
    try:
        inputs = _parse_dictionary(lowered["signature-input"])
        signatures = _parse_dictionary(lowered["signature"])
    except KeyError as error:
        raise SignatureError("signature headers malformed") from error
    now = int(time.time()) if now is None else now
    expected = [*COVERED_COMPONENTS]
    if idempotency_key is not None:
        expected.append(IDEMPOTENCY_COMPONENT)

    for label, entry in inputs.items():
        members, params = entry
        if not isinstance(members, list) or not _covers_exactly(members, expected):
            continue
        created = params.get("created")
        if type(created) is not int or abs(now - created) > MAX_CLOCK_SKEW_SECONDS:
            continue
        if params.get("keyid") != keyid or params.get("alg", "ed25519") != "ed25519":
            continue
        if "nonce" in params and type(params["nonce"]) is not str:
            continue
        signature, signature_params_ = signatures.get(label, (None, {}))
        if not isinstance(signature, bytes) or signature_params_:
            continue
        base = signature_base(method, target_uri, digest, idempotency_key, _sf_serialize([entry]))
        try:
            public_key.verify(signature, base)
        except InvalidSignature:
            continue
        key = _parse_idempotency_key(idempotency_key) if idempotency_key is not None else None
        return VerifiedRequest(keyid=keyid, created=created, idempotency_key=key)
    raise SignatureError("no valid signature")


def _parse_idempotency_key(value: str) -> str:
    parse = cast("Callable[..., object]", http_sf.parse)
    try:
        parsed = parse(value.encode("ascii"), tltype="item")
    except (UnicodeEncodeError, http_sf.StructuredFieldError) as error:
        raise SignatureError("idempotency key malformed") from error
    if not isinstance(parsed, tuple) or type(parsed[0]) is not str or parsed[1]:
        raise SignatureError("idempotency key malformed")
    return parsed[0]


def _sf_serialize(value: object) -> str:
    # http_sf's annotations omit its (value, parameters) tuple data model, so it is typed here.
    serialize = cast("Callable[[object], str]", http_sf.ser)
    return serialize(value)


def _parse_dictionary(value: str) -> dict[str, tuple[object, dict[str, object]]]:
    parse = cast("Callable[..., object]", http_sf.parse)
    try:
        parsed = parse(value.encode("ascii"), tltype="dictionary")
    except (UnicodeEncodeError, http_sf.StructuredFieldError) as error:
        raise SignatureError("signature headers malformed") from error
    entries: dict[str, tuple[object, dict[str, object]]] = {}
    for label, member in cast("dict[str, object]", parsed).items():
        if not isinstance(member, tuple) or len(member) != 2 or not isinstance(member[1], dict):
            raise SignatureError("signature headers malformed")
        entries[label] = (member[0], member[1])
    return entries


def _covers_exactly(members: list[object], expected: list[str]) -> bool:
    """True when the inner list names exactly `expected`, as plain strings without parameters."""
    if len(members) != len(expected):
        return False
    for member, name in zip(members, expected, strict=True):
        if not isinstance(member, tuple) or len(member) != 2:
            return False
        value, params = member
        if type(value) is not str or value != name or params:
            return False
    return True
