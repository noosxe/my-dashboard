# Third-party notices

my-dashboard is MIT-licensed ([LICENSE](LICENSE)). The repository also
carries a few files copied or derived from other projects. They remain
under their original licenses, listed below. Rust crate dependencies are
not vendored here; each is fetched by Cargo under its own license
(`cargo deny list -l license`, policy in `deny.toml`).

When you vendor a new third-party file, add it here in the same PR.

## GPUI Kit (gpui-component) theme files: Apache-2.0

- `crates/core/assets/themes/presets/*.json`: byte-for-byte copies of the
  kit's `themes/` gallery (docs/0074, docs/0078).
- `crates/core/assets/themes/kit-defaults.json`: a byte-for-byte copy of the
  kit's `crates/component/src/theme/default-theme.json` (docs/0078).

Source: <https://github.com/longbridge/gpui-kit>, at the rev pinned in
`Cargo.toml`. Copyright 2024 - 2026 Longbridge. Licensed under the Apache
License, Version 2.0. The upstream license file is reproduced in full in
[LICENSES/gpui-kit-LICENSE-APACHE](LICENSES/gpui-kit-LICENSE-APACHE).
The files are unmodified. The theme palettes they encode credit their
original authors in each file's `author`/`url` fields, which are kept
as shipped.

## GitHub Octicons: MIT

- `crates/app/assets/icons/github.svg`, `lock.svg`, `git-branch.svg`.

Source: <https://github.com/primer/octicons>.

```text
MIT License

Copyright (c) GitHub Inc.

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

The GitHub mark (`github.svg`) is a GitHub trademark. It is used only to
label the GitHub integration.

## Lucide: ISC

- `crates/app/assets/macos/AppIcon.svg` and the generated
  `AppIcon.icns`: the glyph is Lucide's `gallery-vertical-end`
  (docs/0079 D1), which is not one of Lucide's Feather-derived icons.

Source: <https://github.com/lucide-icons/lucide>.

```text
ISC License

Copyright (c) Lucide Icons and Contributors

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
```
