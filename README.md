# diary.sys

A small terminal diary for logging progress. Amber-phosphor look, vim-style keys, plain JSON storage, written in Rust with [ratatui](https://ratatui.rs).

- Multi-line entries, readable in full in a scrollable view
- Edit any entry with a real cursor
- `#tags` written inline, with live filtering
- "Beacon" marker for entries that matter
- Atomic writes: a crash mid-save can't corrupt the log

## Build and install

```sh
cargo build --release
cp target/release/diary ~/.local/bin/    # make sure ~/.local/bin is in your PATH
```

The binary is named after the package in your `Cargo.toml`; adjust the name above if yours differs.

Dependencies: `ratatui`, `crossterm`, `chrono`, `serde` (with `derive`), `serde_json`.

## Where the log lives

The log path is resolved in this order:

1. A path passed as an argument: `diary ~/notes/log.json`
2. The `DIARY_LOG` environment variable
3. `$XDG_DATA_HOME/diary/diary-log.json`, or `~/.local/share/diary/diary-log.json` if `XDG_DATA_HOME` is not set

The folder is created on first run. The log is always the same file no matter which directory you run `diary` from.

To keep it somewhere else (a synced or git-tracked folder, for example), put this in your shell config:

```sh
export DIARY_LOG=~/notes/diary-log.json
```

## Using it

### List

| Key | Action |
| --- | --- |
| `j` / `k`, `↓` / `↑` | Move selection |
| `Enter` | Read the selected entry in full |
| `n` | New entry |
| `e` | Edit the selected entry |
| `/` | Filter by tag |
| `b` | Toggle beacon on the selected entry |
| `d` | Delete the selected entry (asks `y` / `n`) |
| `r` | Reload the log from disk |
| `Esc` | Clear the filter; quit if no filter is active |
| `q` | Quit |
| `Ctrl+C` | Quit from any mode |

Entries are shown newest first. Entries with more than one line show the first line and `+N more lines`.

### Reader (`Enter`)

| Key | Action |
| --- | --- |
| `j` / `k`, `↓` / `↑` | Scroll |
| `PageDown` / `Space`, `PageUp` | Scroll ten lines |
| `g` / `G` | Jump to top / end |
| `h` / `l`, `←` / `→` | Newer / older entry |
| `e` | Edit this entry |
| `b` | Toggle beacon |
| `q`, `Esc`, `Enter` | Back to the list |

### Writing and editing

| Key | Action |
| --- | --- |
| Typing, `Enter`, `Backspace`, `Delete` | Edit at the cursor |
| Arrow keys | Move the cursor (`↑` / `↓` move by visual line) |
| `Home` / `End` | Start / end of the visual line |
| `Tab` | Toggle beacon |
| `Esc` | Go to review (or back to the list if the text is empty) |

In review:

| Key | Action |
| --- | --- |
| `s` | Save |
| `e` | Keep editing |
| `c`, `Esc` | Discard |
| `j` / `k`, `PageUp` / `PageDown` | Scroll to read everything before saving |

Editing an existing entry keeps its number and original date.

If saving fails (read or write error), the text is kept and you stay in review, so you can press `s` again.

### Tags and filtering

Write `#tag` anywhere in an entry, for example:

```text
Finished the parser #rust #diary
```

- Tags are letters, digits, `_` and `-`, and are case-insensitive.
- They are not stored separately; they are read from the text, so editing the text changes the tags.
- Press `/` and type. The list narrows as you type. `Enter` keeps the filter, `Esc` clears it.
- Matching is by prefix: `ru` finds `#rust`.
- Several words mean AND: `rust diary` shows entries that have both tags.
- The header shows the active filter and how many entries match.

## Log format

A JSON array, readable and editable by hand:

```json
[
  {
    "id": "1760000000000-1a2b",
    "num": 1,
    "text": "Finished the parser #rust",
    "ts": 1760000000000,
    "beacon": false
  }
]
```

- `num` is the `LOG#0001` number, `ts` is milliseconds since the Unix epoch, and `beacon` is optional (defaults to `false`).
- Saving writes to `diary-log.json.tmp` next to the log and renames it over the original.
- Before adding or editing an entry, the app re-reads the file, so changes made elsewhere in the meantime are not overwritten.

## Limitations

- No undo.
- Line wrapping counts characters, not display width, so wide characters (CJK, emoji) can look slightly misaligned.
- Everything lives in a single `main.rs`.
