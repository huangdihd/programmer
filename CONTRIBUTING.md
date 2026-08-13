# Contributing to Programmer

Thanks for helping improve Programmer. Bug reports, compatibility findings,
documentation fixes, and focused pull requests are all welcome.

## Before opening an issue

- Search the existing issues first.
- Upgrade to the latest release and check whether the problem still occurs.
- Remove API keys, authorization headers, private prompts, local usernames,
  and proprietary source code from logs and screenshots.
- For provider problems, include the provider name, base URL host (without
  credentials), model ID, and whether `/providers refresh <provider>` works.

Use the bug-report template for reproducible failures and the feature-request
template for proposed behavior. GitHub Issues are the primary support and
feedback channel.

## Development setup

Programmer uses the latest stable Rust toolchain and the Rust 2024 edition.

```sh
git clone https://github.com/huangdihd/programmer.git
cd programmer
cargo build
cargo test --locked --all-features --all-targets
```

The repository architecture and conventions are documented in
[`PROGRAMMER.md`](PROGRAMMER.md). In particular:

- avoid `unwrap()` in production code;
- add the GPL-3.0-or-later header to new Rust source files;
- keep platform-specific behavior working on Windows, macOS, and Linux;
- add or update tests for behavior changes;
- never commit real provider keys or private session data.

## Pull requests

1. Create a focused branch from `develop`.
2. Make the smallest coherent change and add tests where applicable.
3. Run the local checks:

   ```sh
   cargo fmt -- --check
   cargo clippy --locked --all-features --all-targets
   cargo test --locked --all-features --all-targets
   cargo doc --no-deps --all-features
   ```

4. Open the pull request against `develop` and explain the user-visible
   behavior, verification performed, and any provider or platform limitations.

Do not mix unrelated refactors into a bug fix. Screenshots or short terminal
recordings are useful for TUI changes, but redact sensitive information first.

## Reporting provider compatibility

OpenAI-compatible providers vary by endpoint and feature. A useful report says
which of these operations succeeds or fails:

- streamed Responses API text;
- tool calls and tool outputs;
- `GET /models` discovery;
- usage fields such as `input_tokens`;
- image input;
- classifier logprobs.

Include a minimal, redacted error response when possible. A provider can still
be usable when model discovery or an optional capability is unavailable.

## License

By contributing, you agree that your contribution is licensed under the
repository's [GPL-3.0-or-later license](LICENSE).
