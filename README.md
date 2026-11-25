# claudegrep

These four sentences are the only ones written by a human in this entire repo.

It's a mere vibe-coded experiment, which turned out to work good enough to be my daily driver.

If you expect anything of quality, move along.

If you need help, ask your god of choice.

--


claudegrep gives Claude Code a way to look into its own earlier sessions. It searches, lists and reads their transcripts from the command line, so when you point at earlier work ("like last time", "what did we decide about X"), Claude can look it up and read on from there.

Claude Code deletes a transcript 30 days after its last activity (`cleanupPeriodDays`). claudegrep keeps a compressed copy of each one, so those sessions stay searchable. Index, copies and model all stay on your machine.

## How it works

Every command first compares the transcripts under `~/.claude/projects` with the index and handles what is new or changed, so there is nothing to initialise and nothing to keep running. It stores a compressed copy of each transcript and cuts user messages, replies and subagent reports into chunks of at most 1,000 characters. Tool calls and their output can be read but are not searched.

A ColBERT model turns each chunk into many small vectors, one for each token or pair of tokens. A search scores the query's vectors against them, ranks the same chunks by the query's exact words with SQLite's full-text index, and fuses the two rankings. Everything lives in one SQLite file.

## Install

Release archives are built for Linux on x86_64 and for macOS on Apple Silicon. Take yours from the [releases](https://github.com/lmoe/claudegrep/releases) and put the `claudegrep` in it on your `PATH`:

```sh
gh release download --repo lmoe/claudegrep --pattern '*aarch64-apple-darwin.tar.gz'   # Linux: '*x86_64-unknown-linux-gnu.tar.gz'
tar xzf claudegrep-*.tar.gz claudegrep && mv claudegrep ~/.local/bin/
xattr -d com.apple.quarantine ~/.local/bin/claudegrep   # macOS, after a browser download
```

The Linux binary needs glibc 2.34 or newer (Ubuntu 22.04, Debian 12). Anywhere else, `cargo install --path .` in a clone builds it with Rust 1.90 or later. On Linux that also takes a C compiler, `pkg-config` and the OpenSSL headers (`apt install build-essential pkg-config libssl-dev`).

## Use it from Claude Code

Claude uses the tool once it knows that it exists and when to reach for it. Add this to `~/.claude/CLAUDE.md`. It doubles as the list of commands:

```markdown
## claudegrep: earlier Claude Code sessions

`claudegrep` searches, lists and reads the transcripts of earlier Claude Code sessions, including ones Claude Code has already deleted. Use it before asking me or starting over whenever I point at earlier work ("like last time", "the fix from yesterday", "what did we decide about X"), or when an earlier session probably settled what you are about to work out again.

It sees the sessions started in the current directory and the sessions whose last `/cd` went there. When the directory has none, it sees those of its git root. `--all` widens that to every project and goes before or after the subcommand. A session is named by the 8-character id that output prints, or by its number in `list`. Reading a session from another project needs `--all` too.

- `claudegrep "token refresh bug"`: search this project. Meaning and exact words both count, results are grouped by session, and every match comes with the `read --from` command that opens it.
- `claudegrep search status`: the same search, written out. A query that is one of the words `list`, `read`, `search`, `index`, `status`, `config`, `forget` or `help` runs that command, so it needs this form.
- `claudegrep --all "token refresh bug"`: the same over every project.
- `claudegrep -n 3 -C 1 "token refresh"`: at most 3 matches, each with one passage before and after it.
- `claudegrep --since 2w "docker build"`: only sessions active in the last two weeks. `--until` is the other end. Both take a date (`2026-10-14`) or an age (`30m`, `6h`, `2d`, `1w`, `3mo`).
- `claudegrep list`: this project's sessions, newest first.
- `claudegrep read <id> --outline`: one line per turn, to pick where to read.
- `claudegrep read <id> --from 12.0`: read from turn 12, part 0. Each page ends with the command that continues it.
- `claudegrep read <id> --turn 12 --context 1`: turn 12 with one turn either side.
- `--deep` on `read`: tool calls, tool results and thinking as well.
- `--budget N` on `read`: the page size in characters. `claudegrep read <id> --budget 0 > FILE` writes a whole session to a file, to read with the Read tool.

Pick the turns worth reading from a search or the outline before paging through a whole session. `claudegrep --all list` prints every session there is, so search instead. Search skips the session it runs in. Never run `claudegrep forget`: it deletes an archived session for good.
```

No permission rule is needed. Auto mode runs these commands without asking, and the default mode asks once per repository and can remember the answer. A deny rule for `Bash(claudegrep forget:*)` in `~/.claude/settings.json` backs up the section's last sentence.

Two more commands are for you: `claudegrep status` shows what is indexed and where, and `claudegrep config` the settings.

## Settings

Settings are stored in the database. `claudegrep config limit 12` sets one, and `claudegrep config limit --unset` restores its default.

| Setting | Default | What it changes |
|---|---|---|
| `limit` | `8` | Matches a search prints. `-n 20` overrides it once. |
| `context` | `2` | Turns `read --turn 12` shows either side of turn 12. `--context 0` overrides it once. |
| `budget` | `24000` | Characters per page of `read`, `0` for no paging. Claude Code hands Claude 30,000 characters of a command's output and saves the rest to a file, and the default stays below that. |
| `model` | `lightonai/LateOn-Code-edge` | A Hugging Face repository with a `model.onnx`, or a local directory. `lightonai/LateOn-Code` is nine times the size (569 MiB) and makes a search over 8,000 chunks take 1.4 s where the default takes 0.3 s. A change embeds everything again. |
| `device` | `auto` | Where chunks are embedded. `auto` uses a GPU when there is one and stores `gpu` once that has worked. `cpu` never uses one. |
| `threads` | half the cores | Threads for parsing and for embedding on the CPU. |
| `projects-root` | `~/.claude/projects` | Where Claude Code keeps its transcripts. `CLAUDE_CONFIG_DIR` moves the default. |

## Downloads and files

`list` and `read` work offline. The first `search` or `index` downloads what it needs from PyPI and Hugging Face, checks every wheel against a pinned SHA-256 and embeds every session once, which took 50 seconds for 230 sessions on 12 CPU threads.

| Download | Size | When |
|---|---|---|
| ONNX Runtime 1.32.0 | 25 MiB | always |
| The model | 70 MiB | always |
| ONNX Runtime for CUDA | 240 MiB | Linux on x86_64 with an NVIDIA driver, in place of the CPU build |
| CUDA 13, cuDNN 9 | 500 and 490 MiB | the same, each only if the machine does not have it |

When a GPU was to be used and cannot be, one line on stderr says why and the CPU does the work. On Apple Silicon the standard runtime embeds through CoreML, which is untested. `HTTPS_PROXY` is honoured, and `ORT_DYLIB_PATH` names an ONNX Runtime of your own (1.23 or later).

The database is `~/.local/share/claudegrep/index.sqlite` (macOS: `~/Library/Application Support/claudegrep/index.sqlite`). `--db FILE` or `CLAUDEGREP_DB` names another. Deleting it loses the settings and the copies of sessions Claude Code has already deleted. Runtimes are cached in `~/.cache/claudegrep/` (macOS: `~/Library/Caches/claudegrep/`), the model in `~/.cache/huggingface/`.

## Develop

`cargo fmt`, `INSTA_UPDATE=no cargo test --no-fail-fast` and `cargo clippy --all-targets` are what CI runs. The search tests use the real model and download it on their first run. A tag `vX.Y.Z` that matches `Cargo.toml` builds the release archives.

## License

`LGPL-3.0-or-later`, without any warranty. See `COPYING.LESSER` and `COPYING`.
