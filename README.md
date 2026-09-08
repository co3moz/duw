# duw

`du`, but the report lands in your browser while the scan is still running.

```bash
cargo install duw
duw            # scans the current directory and opens the UI
duw /var/log
```

`duw` walks the tree on all your cores, aggregates sizes upwards as it goes and
streams progress to a local page. You can click into folders, sort by apparent
or on-disk size, and see where the space went before the scan has finished.

![The duw report: a folder list on the left, a treemap on the right](docs/screenshot.png)

## The UI

- **Folders**, every entry in the current directory, largest first, with a
  share bar. Directories that usually hold regenerable data (`node_modules`,
  `target`, `.git`, `__pycache__`, …) get a badge.
- **File types**, the whole subtree grouped by category and extension.
- **Largest files**, the hundred biggest files anywhere below the current
  directory.
- **Treemap**, three levels deep, click a rectangle to descend. Trimmed
  entries show up as an explicit "… N more" cell rather than inflating their
  siblings.

`Backspace` or `Escape` goes up a level. Nothing is uploaded anywhere; the
server binds to `127.0.0.1` and stops when you press `Ctrl+C`.

## Apparent vs on-disk size

The toggle in the header switches between the two, the way `du --apparent-size`
and plain `du` differ:

| | apparent | on disk |
|---|---|---|
| Unix | `st_size` | `st_blocks × 512`, exact |
| Windows | file size | rounded up to the volume cluster size, an estimate |

Windows has no cheap per-file allocation size, so `duw` rounds instead of paying
an extra syscall per file. That is exact for ordinary files and overstates
compressed or sparse ones; the UI marks the column with `*` when it applies.

Hard links are counted once on Unix (pass `-l` to count every link, like `du -l`).
Windows does not expose link counts cheaply, so every link is counted there.

## Options

```
duw [OPTIONS] [PATH]

  -p, --port <PORT>        Port to listen on (0 picks a free one)
      --host <HOST>        Address to bind [default: 127.0.0.1]
      --no-open            Do not open a browser window
  -x, --one-file-system    Skip directories on other filesystems (Unix only)
  -L, --dereference        Follow symbolic links
  -l, --count-links        Count hard-linked files once per link
  -d, --max-depth <N>      Do not descend more than N levels
      --exclude <PATTERN>  Exclude entries matching a glob (repeatable)
  -X, --exclude-from <F>   Read exclude patterns from a file
  -j, --threads <N>        Scanning threads [default: number of cores]
```

```bash
duw --exclude '*/node_modules' --exclude '.git' ~/src
duw -d 2 --no-open -p 8080 /
```

## HTTP API

The page is a client of a small JSON API, so the same data is scriptable:

| Endpoint | Returns |
|---|---|
| `GET /api/state` | root path, platform capabilities, live counters |
| `GET /api/events` | SSE stream of `progress` / `done` frames |
| `GET /api/node/{id}` | one directory's children, largest first |
| `GET /api/tree/{id}` | nested slice for the treemap |
| `GET /api/types/{id}` | subtree totals per extension |
| `GET /api/largest/{id}` | biggest files in the subtree |
| `GET /api/errors` | entries that could not be read |
| `POST /api/cancel` | stop the walk, keep the results |

Node `0` is always the scan root. `metric=size\|alloc` selects apparent or
on-disk sizes; `version` increases on every batch of newly scanned entries,
which is how the UI knows when to refetch.

## Building from source

The web UI is a Vite + React app whose build output is embedded in the binary,
so a release build needs no Node at install time, `web/dist` is committed.

```bash
cd web && npm install && npm run build   # only after changing the UI
cargo build --release
```

For UI work, run the backend and the dev server side by side; Vite proxies
`/api` to port 8080:

```bash
cargo run -- --port 8080 --no-open ~/src
cd web && npm run dev
```

Debug builds read `web/dist` from disk, release builds embed it.

## License

MIT
