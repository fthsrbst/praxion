# SETUP.md — one-command setup for the Demo-0 body and the 2.7 spikes

Everything the first Demo-0 work and the session 2.7 experiments need on a fresh machine, in one
idempotent command (including the body release build). Windows 11 + Git Bash is the main target; macOS works too (no `hands` crate,
no dlltool fix).

```sh
python setup.py            # set up, then build
python setup.py --check    # report only, change nothing; exit 1 if a step is missing or wrong
```

Run it from anywhere; it resolves every path from its own location. Needs Python 3.11+ and uses
the standard library only.

## Layout it creates

| Path | What | Committed |
|---|---|---|
| `third_party/azalea/` | Azalea clone at the pinned commit, with the pinned patches applied | no (ignored) |
| `third_party/toolchain/` | dlltool shim for the pinned toolchain | no (ignored) |
| `third_party/minecraft/server-26.3.jar` | Dedicated server jar, sha1 and size verified | no (ignored) |
| `versions/patches/*.patch` | Local Azalea patches, in sorted apply order | yes |
| `local/paths.toml` | Absolute paths the later steps read (`java`, `server_jar`, `azalea_dir`, `toolchain_dir`, `repo_root`) | no (ignored) |
| `spikes/round0/server/` | Server run directory with `eula.txt`, `server.properties` and the world | no (ignored) |
| `spikes/.cargo/config.toml`, `body/.cargo/config.toml` | Generated toolchain fix (linker and dlltool) | no (ignored) |

Product code never depends on `spikes/`; it reads the pinned inputs from `third_party/` and
`local/paths.toml`. Some product tests read machine-local captures and truth files under `spikes/`
(`decoder`, `perception`, `planner`, `receipts`); those tests skip when the ignored files are
absent. `spikes/` keeps only the experiment code and its run directory.

## What it does (ten steps, in order)

| # | Step | What it leaves on disk |
|---|---|---|
| 1 | Prerequisites | nothing; checks `git` and the Python version |
| 2 | Rust | rustup (if missing) and the toolchain pinned in `rust-toolchain.toml`, minimal profile + `llvm-tools` |
| 3 | dlltool shim (Windows) | `third_party/toolchain/llvm-dlltool.exe`, a copy of the sysroot's `llvm-ar.exe` |
| 4 | cargo config (Windows) | `spikes/.cargo/config.toml` and `body/.cargo/config.toml`, each with the same absolute `rust-lld` and dlltool paths |
| 5 | Azalea | `third_party/azalea` at the pinned commit with every `versions/patches/*.patch` applied |
| 6 | Java 25 | nothing; finds and reports a Java 25 runtime |
| 7 | Local paths | `local/paths.toml` with the five absolute paths |
| 8 | Server | `third_party/minecraft/server-26.3.jar` (sha1 and size verified), `spikes/round0/server/`, its `server.properties` keys |
| 9 | Client files | nothing; checks the launcher's 26.3 install (version JSON and jar sha1) |
| 10 | Build | `cargo build --release` in `body/` (the `praxion-body`, `praxion-forwarder` and `praxion-headless` binaries the harness runs), in `spikes/decode` and (Windows only) in `spikes/hands` |

Every step checks first. On a finished machine all of them print `ok: ...` and nothing changes.
The build step delegates freshness to cargo, which is a no-op when the binaries are current.

## Flags

| Flag | Effect |
|---|---|
| `--check` | report every step as ok / missing / wrong, change nothing, exit 1 if anything is missing or wrong |
| `--no-build` | skip step 10 (`cargo build` and `cargo test`) |
| `--test` | after building, run `cargo test --release` in the body workspace and every spike crate |
| `--accept-eula` | write `eula=true` in `spikes/round0/server/eula.txt` (read https://aka.ms/MinecraftEULA first) |
| `--java PATH` | use this Java 25 binary instead of the launcher runtime |

`--check` ignores `--no-build` and `--test`: it never compiles.

## What it never does

- **No EULA acceptance without `--accept-eula`.** Without the flag the server step prints the EULA
  notice and continues; the run still succeeds, and `--check` reports a warning, not an error.
- **No client download.** Step 9 only verifies what the official launcher installed.
- **No system settings.** No PATH, power or registry changes, and nothing is written outside this
  repository. Downloads come only from piston-meta (server jar), static.rust-lang.org (rustup-init)
  and the Azalea GitHub repository (`git clone`).
- **No game or server start**, and no ports are touched.
- **No foreign changes.** It stops if `third_party/azalea` has uncommitted changes outside
  `versions/patches/*.patch`.

## Rerunning

Rerun after pulling, after editing `versions/patches/`, or whenever `--check` names a missing step.
Nothing needs cleaning up: done steps are skipped, wrong ones are fixed.

- Rust: a missing toolchain or `llvm-tools` is installed by a plain rerun.
- Azalea: delete `third_party/azalea` and rerun; it is a disposable clone at the pinned commit.
- Server jar: a wrong sha1 or size is re-downloaded automatically.
- Local paths: rerun after moving the repository or installing a different Java 25.
- Java: install the official launcher and run 26.3 once, or pass `--java`.

## Pins

Every pinned version, hash and URL lives in `versions/manifest.toml`, the one pin source shared with
`body/` and `harness/`; the comment above each value says where it comes from. Read a pin there and
change it there, never in a generated file. The script keeps only setup-owned values, such as the
`server_properties` keys in `PINS`. `spikes/.cargo/config.toml` and `body/.cargo/config.toml` are
generated by the script and git-ignored.
