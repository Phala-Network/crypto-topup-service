## Summary

<!-- What changes and why. Link the issue it resolves, for example "Closes #123". -->

## Specification

<!-- The sections of docs/architecture.md this implements or changes, or "none". -->

## Verification

<!-- The commands you ran and their results, for example `make test` or `make sdk-check`. -->

## Checklist

- [ ] The title is a Conventional Commit: `<type>(<scope>): <summary>`.
- [ ] Tests cover the change and fail if the behavior is removed.
- [ ] Documentation is updated, and changelog entries are added under `## [Unreleased]`
      (`CHANGELOG.md` for API or webhook changes, the SDK's `CHANGELOG.md` for SDK changes).
- [ ] No secret, key, or credential can reach a log, error, or response.
- [ ] Migrations, if any, are additive and reversible.
