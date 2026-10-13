# Bistro

This is Bistro.

Bistro shows your [Herdr](https://herdr.dev) agents in one tall list. It is made for vertical monitors. It looks like the menu board outside a bistro.

![Bistro with three hosts](docs/screenshots/bistro-3-hosts.png)

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/evanbrobertson/bistro/main/install.sh | sh
```

This installs `bistro` to `~/.local/bin`. Make sure that folder is in your `PATH`.

To get the beta version:

```sh
curl -fsSL https://raw.githubusercontent.com/evanbrobertson/bistro/main/install.sh | BISTRO_CHANNEL=beta sh
```

To install to a different folder, set `BISTRO_INSTALL_DIR`.

## Use

Herdr must be installed. Run:

```sh
bistro
```

- Click an agent to jump to it in Herdr.
- Right-click an agent for a menu. Choose **Pin** to keep it at the top.
- Bistro updates when Herdr says something changed. It does not poll.
- Bistro remembers your pins and panel sizes in `~/.config/bistro/state.json`.

## Develop

You need [Rust](https://rustup.rs).

```sh
cargo run              # run Bistro
cargo fmt --all        # format
cargo clippy --all-targets -- -D warnings
cargo test
```

The screenshots in `docs/screenshots` use made-up data. To make them again:

```sh
cargo test screenshots -- --ignored
for n in 1 2 3; do rsvg-convert -z 1 docs/screenshots/bistro-$n-hosts.svg -o docs/screenshots/bistro-$n-hosts.png; done
```

Rules for changes:

- Open a pull request. Do not push to `main`.
- Write the pull request title as a [Conventional Commit](https://www.conventionalcommits.org), for example `feat: add a filter`. The title sets the next version.
- `feat` makes a minor version. `fix` and `perf` make a patch version. `!` (for example `feat!:`) makes a breaking change.

## Releases

Every merge to `main` that has a `feat`, `fix`, `perf`, or breaking change makes a new release. Nothing is released for `docs`, `chore`, `ci`, `test`, `style`, or `refactor`.

| Branch            | What it holds                                |
| ----------------- | -------------------------------------------- |
| `main`            | Trunk. All work lands here.                  |
| `release/develop` | Always the same as `main`.                   |
| `release/beta`    | The newest release.                          |
| `release/stable`  | The newest release you promoted by hand.     |

To move a release to stable, go to **Actions**, pick **Promote**, and enter the tag (for example `v0.2.0`). Nothing is rebuilt. The same files that were in beta become stable.

## License

MIT
