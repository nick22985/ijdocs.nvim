# ijdocs

> **Early stage / experimental** — this project is under active development. APIs, config, and behavior may change without notice.

IntelliJ-style javadoc hover and browser docs for Neovim, backed by a tiny Rust HTTP server that serves the `*-javadoc.jar` files already cached by Maven and Gradle.

## What it does

- **Rich floating javadoc** — hover on any Java symbol to get its javadoc rendered in a floating window with treesitter Java syntax highlighting, parameter annotations (`@NotNull`, `@Nullable`), and navigable type references.
- **In-float navigation** — press `K` on any highlighted type name to jump to its docs, `BS` to go back. Works across packages and JARs.
- **Browser fallback** — press `gK` or `Enter` to open the full HTML javadoc in your system browser.
- **Source file support** — hovering symbols defined in your project (not JARs) extracts javadoc from the `.java` source file directly.

Symbol resolution goes through `jdtls` (`textDocument/definition` returns a `jdt://` URI which we parse into an FQN and jar hint), then the FQN is fetched from the local Rust server which extracts the matching HTML out of the appropriate javadoc JAR. The jar hint from the jdt URI ensures the correct dependency version is used.

## Float keybindings

| Key     | Action                                          |
| ------- | ----------------------------------------------- |
| `K`     | Navigate to docs for the type under cursor      |
| `BS`    | Go back to previous doc                         |
| `gK`    | Open type in browser                            |
| `Enter` | Open in browser (JAR classes) or file (sources) |
| `q`     | Close float                                     |
| `Esc`   | Close float                                     |

## Features

- Extracts parameter annotations (`@NotNull`, `@Nullable`, etc.) from bytecode (classes JARs) and injects them into method signatures
- Resolves type cross-references from javadoc HTML links — highlighted in blue and navigable via `K`
- Annotation types are also navigable (e.g. jump to `@NonNull` docs)
- Handles both Maven (`~/.m2/repository`) and Gradle (`~/.gradle/caches`) cached JARs
- Correct version resolution via jar hints from jdt:// URIs
- Source file javadoc extraction with parameter-level member lookup

## Setup

### 1. Build and run the server

```bash
cargo build --release
./target/release/ijdocs-server          # listens on 127.0.0.1:63343
```

By default the server scans `~/.m2/repository` and `~/.gradle/caches/modules-2/files-2.1`. Add more roots with `--extra-root /some/path`.

To keep it always running, drop a systemd user unit at `~/.config/systemd/user/ijdocs.service`:

```ini
[Unit]
Description=ijdocs javadoc server

[Service]
ExecStart=%h/projects/personal/ijdocs/target/release/ijdocs-server
Restart=on-failure

[Install]
WantedBy=default.target
```

Then `systemctl --user enable --now ijdocs`.

### 2. Install the nvim plugin

With lazy.nvim, point at the local checkout:

```lua
{
  dir = "~/projects/personal/ijdocs/nvim",
  ft = "java",
  config = function()
    require("ijdocs").setup({
      url = "http://127.0.0.1:63343",
    })
  end,
  keys = {
    { "<C-k>", function() require("ijdocs").hover() end, ft = "java", desc = "ijdocs: hover" },
    { "gK", function() require("ijdocs").open_browser() end, ft = "java", desc = "ijdocs: browser" },
  },
}
```

You need `nvim-jdtls` already attached to your Java buffers — this plugin uses jdtls's `textDocument/definition` to resolve the FQN.

### Configuration

```lua
require("ijdocs").setup({
  url = "http://127.0.0.1:63343",  -- server URL
  server_bin = nil,                  -- path to binary (auto-detected from plugin dir)
  auto_start = true,                 -- start server automatically if not running
  browser_cmd = nil,                 -- browser command (defaults to xdg-open / open)
  float = {
    border = "rounded",              -- float border style
    max_width = 100,                 -- max float width
    max_height = 30,                 -- max float height
  },
})
```

## Endpoints (for direct curl / debugging)

- `GET /md/<fqn>` — markdown + type_refs + annotations for nvim float (`?member=foo` to narrow to one method, `?jar=hint` for version disambiguation)
- `GET /html/<fqn>` — 307 redirect to the canonical JAR-served HTML page
- `GET /jar/<artifact>/<path>` — serves files from inside javadoc JARs (HTML, CSS, images)
- `GET /search?q=<substring>&limit=N` — package/class search
- `GET /source?path=/path/to/File.java&member=foo` — extract javadoc from a local source file
- `GET /health` — health check
