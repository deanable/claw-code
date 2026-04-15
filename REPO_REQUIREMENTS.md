# Repository Requirements

This document captures the working rules for future changes in this repository.

## Release And Packaging

- The release workflow must be Windows-only.
- GitHub releases should publish the Windows installer executable, not raw source or standalone binaries.
- The release asset should be the Windows installer executable only.
- The installer should include `claw.exe`, `claw-launcher.exe`, the required `.claw` payload, and the user-facing `README.txt`.
- The installer should create a desktop shortcut to `claw-launcher.exe`.
- The installer should prefer a per-user install location so the launcher can write its state without requiring elevation.
- Keep the release workflow tied to version tags pushed to `main`.
- Do not add back macOS or Linux release jobs unless explicitly requested.
- GitHub will still expose auto-generated source archives for tags; the workflow itself should not upload source bundles.
- Do not publish separate source archives or standalone binaries from the workflow.
- Treat the installer as the release deliverable, not the repository source tree.

## Windows Behavior

- The application must assume Windows-native execution on Windows hosts.
- Prefer PowerShell or the native terminal on Windows.
- Do not default to Bash on Windows.
- The launcher should honor the directory it was started from as the working directory.
- The launcher should send the initial instructions as the first command when launch text is present.
- If shell selection matters on Windows, prefer PowerShell or the native terminal before Bash.
- Avoid repeated fallback attempts that only discover the wrong shell after several failures.

## Versioning

- Every release-prep commit should include a version bump.
- Keep `rust/Cargo.toml` and `rust/Cargo.lock` aligned with the release version.
- Tag releases from `main` after the version bump lands.
- Use annotated version tags that match the release number.

## Repo Style And Expectations

- Keep changes focused and incremental.
- Preserve the current Windows-first workflow unless a request explicitly broadens scope.
- Avoid adding unrelated cleanup or feature work while fixing a release issue.
- When a release workflow fails, inspect the actual GitHub Actions run before changing code.
- If a change affects release behavior, document the intent in the repository so future pull requests follow the same rules.
- Prefer making the repo state obvious over leaving hidden conventions in chat history.
- Keep the source of truth on `main`.
- Remove dead branches and unneeded release paths instead of carrying them forward.

## Troubleshooting Preference

- Prefer fixing the workflow and packaging setup before changing repository layout.
- Prefer installer output over publishing extra artifacts.
- If a platform-specific issue appears, resolve it at the platform boundary instead of adding cross-platform complexity.
- When documentation and workflow behavior conflict, update both so they match.
- When release failures happen, read the actual workflow result first and only then adjust code.
- If the problem is not in the current diff, call that out clearly before changing more code.
