# SHAGADELIC working guidelines

- Read [the design](docs/research/SHAGADELIC.md) before structural changes. Update it when a contract or implementation milestone changes.
- Keep commits focused and describe the behavior they introduce. Use Cargo for builds and checks. Run `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`.
- Use the Washoi TypeScript bot runner as the compatibility reference. Record the source path when porting code. Verify wire behavior with fixtures and reuse compatible headless SHAGGER APIs where practical.
- Keep Washoi rules and bot behavior in its integration. Preserve the lobby's authority over admission and shutdown. Keep credentials out of logs and fixtures, and limit bot decisions to information the bot could perceive.
