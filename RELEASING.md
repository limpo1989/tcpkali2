# Release Process

Releases are automated by `.github/workflows/publish.yml`. A release is
triggered only by pushing a tag whose name is exactly `v` followed by the
version in `Cargo.toml`.

## Prerequisites

- Configure the repository secret `CARGO_REGISTRY_TOKEN` with permission to
  publish `tcpkali2` on crates.io.
- Keep `Cargo.lock` committed.
- Do not reuse or move a published version tag.

## Prepare a Release

1. Update `package.version` in `Cargo.toml` and update `Cargo.lock`.
2. Run the same checks used by CI:

   ```bash
   cargo fmt --all -- --check
   cargo clippy --all-targets --locked -- -D warnings
   cargo test --all-targets --locked
   cargo check --all-targets --locked
   cargo build --release --locked
   cargo package --allow-dirty --locked
   ```

3. Commit and push the release changes to `master`.
4. Wait for every job in the `CI and Release` workflow to succeed.
5. Create and push an annotated tag that matches the Cargo version:

   ```bash
   git tag -a v0.3.0 -m "Release v0.3.0"
   git push origin v0.3.0
   ```

## Automated Gates

The tag workflow will not publish the crate until all of these gates pass:

- tag and Cargo version validation;
- formatting, Clippy, unit/integration tests, and crate package verification;
- Rust 1.88 minimum-supported-version check;
- release binary builds for Linux x86-64, macOS arm64, and Windows x86-64;
- binary archive upload and draft GitHub Release creation.

After the gates pass, the workflow publishes the crate to crates.io and then
makes the draft GitHub Release public. If crate publication fails, the draft
release is removed. A crates.io version cannot be overwritten or rolled back;
if the final GitHub Release step fails after crate publication, rerun that
failed job rather than reusing or moving the tag.
