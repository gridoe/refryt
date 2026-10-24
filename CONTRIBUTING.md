# Contributing

Bug reports, ideas and small fixes are welcome. Larger changes start as an issue, so the approach can be agreed before the work is done.

## Reporting a bug

Open a [bug report](https://github.com/gridoe/refryt/issues/new?template=bug.yml). It asks for:

- The version and the package, for example "0.1.10, Windows ARM64 MSI" or "0.1.10, Flatpak". The version is shown in Settings.
- What happened, what was expected, and the steps to get there.
- The end of `refryt.log`, which is in the config folder:

| Platform | Config folder |
|---|---|
| Linux | `~/.config/refryt` |
| Flatpak | `~/.var/app/io.github.gridoe.refryt/config/refryt` |
| macOS | `~/Library/Application Support/refryt` |
| Windows | `%APPDATA%\refryt\config` |

The log is safe to share: it has no login details in it. It does show which songs were played, and folder paths in it can include your username, so cut anything you would rather keep private. Never post `browser.json` or `oauth.json` from the same folder: they hold the signed-in session and work like a password.

After a crash, the app offers the report on the next launch and can open it as a prefilled issue. That is the quickest way to report one.

When a page or playback suddenly stops working for everyone, YouTube has usually changed something on its side. Check the open issues first.

## Ideas

Open a [feature request](https://github.com/gridoe/refryt/issues/new?template=feature.yml) describing what it is for. For anything bigger than a fix, wait for a reply before writing code.

## Pull requests

- Keep each pull request to one change.
- `just check` has to pass. It runs `cargo fmt --check`, clippy with `-D warnings` for both resolver feature sets, and the tests, the same gate a release goes through. CI runs these checks on every pull request on Linux, along with clippy for Windows and clippy and the tests on macOS, so code for a platform you cannot build on is still checked.
- Follow the style of the code around the change, including how much it is commented.
- For a change to the interface, add a screenshot from before and after.

[docs/BUILDING.md](docs/BUILDING.md) covers the setup for each platform. A new theme is a single CSS file; [docs/THEMES.md](docs/THEMES.md) describes the format.

## Licence

refryt is licensed under the [GPL-3.0-or-later](LICENSE), and contributions are accepted under the same licence.
