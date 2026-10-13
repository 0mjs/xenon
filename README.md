# Xenon

A fast, minimal database client. Rust + GPUI. By Carbon Software.

Early prototype. See [PLAN.md](PLAN.md).

## Run

```sh
cargo run -p xenon                     # empty window
cargo run -p xenon -- path/to/file.db  # open a SQLite database
```

You can also open a database with `⌘O` or by dropping the file on the window.

Requires macOS with Xcode (for Metal) and Rust 1.85+ (edition 2024).

| Shortcut | Action |
|---|---|
| `⌘O` | Open SQLite file |
| `⌘↵` | Run the selection, or the whole editor |
| `⌘.` | Cancel the running query |
| `⌘B` | Toggle explorer |
| `⌘I` | Toggle inspector |
| `⌘Q` | Quit |

Fonts: Archivo and IBM Plex Mono, bundled under the SIL Open Font License (`assets/fonts`).
