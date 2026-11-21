# Themes

Every colour theme in refryt is a `.css` file in the themes folder, including the ones the app
ships with. The first time refryt starts, it writes its 29 themes there (Classic, Catppuccin,
Nord, Gruvbox, Tokyo Night, Dracula, Rosé Pine, Everforest, Solarized, macOS, Windows and Breeze, each
light and dark flavour as its own file, and Spotify). A fresh install starts on macOS Dark on a
Mac, Windows Dark on Windows, and Classic Dark elsewhere. From then on the folder is all there is:

- **Delete a file** and that theme is gone. Nothing puts it back.
- **A theme added in a newer release** is written into the folder once. A hidden `.offered` file
  in the folder records which built-in themes it has been given, so a deleted one stays deleted.
- **Edit a file** and that theme changes. Save it while refryt is open, then come back to the
  window (or open Settings) and the app repaints. It looks at the folder at those moments, not on
  a timer, so an idle refryt does nothing in the background.
- **Add a file** and it shows up in **Settings → Theme** under its file name.

The folder is:

- Linux: `~/.config/refryt/themes/`
- Windows: `%APPDATA%\refryt\config\themes\`

**Settings → Open Themes Folder** opens it. The shipped files are the best examples to copy.

Each theme is dark or light, set by its `color-scheme` line. The Theme dropdown lists the
dark themes while the app is in dark mode and the light ones in light mode, and remembers one
pick for each; **Mode** (System / Dark / Light) chooses between them. If the folder has no
theme of the mode on screen, a built-in Classic paints until you add one.

## The file

```css
/* midnight.css */
:root {
  color-scheme: dark;
  --base: #16161e;
  --text: #c0caf5;
  --accent: #ff9e64;
}
```

Only `--base`, `--text` and `--accent` are required. Everything else is derived from them the
same way the shipped themes fill in what their upstream palettes leave out. You set only what
you want to change.

This is CSS *syntax*, not a stylesheet: refryt draws its own interface, so there are no
selectors to match. Declarations inside `:root { }`, any other block, or no block at all all
mean the same thing. `/* comments */` are fine.

Colours can be written `#rgb`, `#rrggbb`, or `rgb(r, g, b)`.

| Variable | What it paints | When left out |
| --- | --- | --- |
| `--base` | The cards: most of the app | required |
| `--text` | Body text | required |
| `--accent` | Play buttons, selection, switches, links | required |
| `--gutter` | The gap behind the cards | `--base`, darkened |
| `--popup` | Menus and popups | between `--gutter` and `--base` |
| `--surface` | Chips, inputs, the search field | `--base` stepped toward `--text` |
| `--surface-hover` | A hovered surface | `--surface` stepped toward `--text` |
| `--surface-active` | A pressed or selected surface | a larger step |
| `--outline` | Hairlines | a small step from `--surface` |
| `--subtext` | Second-rank text: artists, albums | `--text` faded toward `--base` |
| `--muted` | Third-rank text: counts, help lines | faded further |
| `--accent-hover` | A hovered accent | `--accent` lightened (dark) or darkened (light) |
| `--on-accent` | Text and icons on the accent | black or white, whichever reads better |
| `--danger` | Errors | a red |
| `--warning` | Warnings | an amber |

`color-scheme: dark` or `light` says which mode the theme belongs to. Without it, refryt guesses
from `--base`.

## When something is off

- A file that can't be read as a theme is listed in red under the Theme row with the line
  and the reason, e.g. `midnight.css: line 4: tomato is not a colour`. The other themes still
  load.
- If the theme you picked is deleted or stops loading, another theme of the same mode paints
  and Settings says so. Your pick is kept, so restoring the file brings it back.
- Unknown variables are ignored, and body text that's hard to read on `--base` (under 4.5:1)
  gets a warning. Both are shown under the Theme row while that theme paints; neither
  stops the theme from applying.
- Transparency still works. It is applied on top of whatever theme is chosen.
- `refryt --scheme catppuccin-frappe` paints a theme for one run without changing your
  settings. The name is matched ignoring case, spaces, hyphens and accents.
