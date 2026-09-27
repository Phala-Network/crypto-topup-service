# Security

## Report a vulnerability

Report vulnerabilities through
[GitHub private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
for this repository. If it is unavailable, contact security@phala.network.

Do not open public issues for exploitable vulnerabilities, in particular anything that could move
or strand funds, credit a deposit that was not paid, bypass attestation or admin authentication,
or expose keys derived in the CVM.

## Trust boundary

What a deployment guarantees is what its attestation proves: the image digests and settings
measured into the CVM, verified as described in [deploy/README.md](deploy/README.md). Code or
configuration in this repository that is not measured into a deployment is not a guarantee of it.
