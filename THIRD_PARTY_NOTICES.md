# Third-party notices

## CodeZeno/Claude-Code-Usage-Monitor v2.23.0

https://github.com/CodeZeno/Claude-Code-Usage-Monitor — MIT License,
Copyright (C) 2026 Code Zeno Pty Ltd.

Used in AI Usage Lite:

- Vendored with minimal adaptation: `src/poller/claude_desktop.rs`,
  `src/poller/windows_credentials.rs`, `src/poller/claude/limits.rs`, and the
  ISO 8601 parser from `src/poller.rs`.
- Endpoints, request headers and response parsing followed (rewritten):
  `src/poller/claude.rs`, `src/poller/codex.rs`, `src/poller/antigravity.rs`.
- Removed on purpose: the Messages API header probe and CLI-based token
  refreshes (`claude -p .`, `codex exec .`), all of which spend usage.

The upstream user interface, theme engine and updater are not used.

Full upstream license text:

```
MIT License

Copyright (C) 2026 Code Zeno Pty Ltd

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Rust crates

ureq, serde, serde_json, windows, windows-numerics, native-tls and their
dependencies are used under their respective MIT / Apache-2.0 licenses
(see `Cargo.lock`).
