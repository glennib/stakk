#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["typer"]
# ///
"""Re-record the README demo gif (media/stakk.gif) fully automatically.

Resets the playground repo (local bookmarks, remote branches, open PRs),
seeds a deterministic demo graph, records a scripted `stakk` session with
asciinema inside a tmux pane, and converts the cast to a gif with agg.

The choreography below is coupled to the select TUI's user interface:
shortcut keys, navigation directions, the Space state-cycle order, screen
titles, and stakk's final output strings. If any of those change, update
the key sequences and poll strings in this script (see CLAUDE.md,
"Demo recording").
"""

from __future__ import annotations

import json
import random
import re
import shlex
import subprocess
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path

import typer


def log(message: str = "") -> None:
    """Progress reporting goes to stderr; stdout stays clean."""
    typer.echo(message, err=True)


STAKK_ROOT = Path(__file__).resolve().parents[1]

# Terminal theme embedded into the cast header (agg reads it from there).
# Copied from the original ghostty-recorded media/stakk.asciinema.
THEME = {
    "fg": "#ffffff",
    "bg": "#282c34",
    "palette": ":".join(
        [
            "#1d1f21", "#cc6666", "#b5bd68", "#f0c674",
            "#81a2be", "#b294bb", "#8abeb7", "#c5c8c6",
            "#666666", "#d54e53", "#b9ca4a", "#e7c547",
            "#7aa6da", "#c397d8", "#70c0b1", "#eaeaea",
        ]
    ),
}


@dataclass
class SeedCommit:
    """One commit of the demo graph.

    `files` maps path -> full file content at this commit (overwrites are
    how a "modified" file is expressed). `ref` names the commit so later
    commits can use it as `parent`; `parent` refers to a ref or "main".
    """

    ref: str
    parent: str
    message: str
    files: dict[str, str] = field(default_factory=dict)
    bookmark: str | None = None


# The demo graph. The auth/email path is the one selected in the recording;
# only `auth` is bookmarked there so the TUI shows bare rows to play with.
# The side stacks exist to make screen 1 worth browsing.
SEED: list[SeedCommit] = [
    # auth/email path (the demo target)
    SeedCommit(
        ref="auth",
        parent="main",
        message=(
            "Add user authentication module\n\n"
            "Implement basic auth service with login/logout.\n"
        ),
        files={"auth.ts": "export function login(user: string) {}\n"},
        bookmark="auth",
    ),
    SeedCommit(
        ref="logout",
        parent="auth",
        message="Add logout functionality\n\nExtend auth service with session cleanup.\n",
        files={
            "auth.ts": (
                "export function login(user: string) {}\n"
                "export function logout(user: string) {}\n"
            )
        },
    ),
    SeedCommit(
        ref="password",
        parent="logout",
        message="Add password validation\n\nImplement strength checking and hashing.\n",
        files={
            "password.ts": "export function validatePassword(pwd: string): boolean {}\n"
        },
    ),
    SeedCommit(
        ref="verification",
        parent="password",
        message="Add email verification\n\nSend confirmation links to new accounts.\n",
        files={"email.ts": "export function sendVerificationEmail(email: string) {}\n"},
    ),
    SeedCommit(
        ref="templates",
        parent="verification",
        message="Add email templates\n\nCreate HTML templates for verification emails.\n",
        files={"templates.ts": 'export const VERIFICATION_TEMPLATE = "";\n'},
    ),
    SeedCommit(
        ref="smtp",
        parent="templates",
        message=(
            "Add SMTP configuration\n\n"
            "Configure email provider credentials and settings.\n"
        ),
        files={
            "email.ts": (
                "export function sendVerificationEmail(email: string) {}\n"
                'export const SMTP_HOST = process.env.SMTP_HOST || "localhost";\n'
            ),
            "index.ts": (
                "export { AuthAPI } from './api';\n"
                "export { UserRepository } from './database';\n"
                "export { sendVerificationEmail } from './email';\n"
                "export { RateLimiter } from './ratelimit';\n"
            ),
        },
    ),
    # api stack, off the auth commit
    SeedCommit(
        ref="api",
        parent="auth",
        message="Create REST API endpoints\n\nAdd login and user registration endpoints.\n",
        files={"api.ts": "export class AuthAPI {}\n"},
    ),
    SeedCommit(
        ref="api_errors",
        parent="api",
        message="Add API error handling\n\nStandardize error responses across endpoints.\n",
        files={"api.ts": "export class AuthAPI {}\nexport class APIError {}\n"},
    ),
    SeedCommit(
        ref="api_logging",
        parent="api_errors",
        message="Add request logging\n\nLog all authentication attempts.\n",
        files={
            "logging.ts": (
                "export function logAuthAttempt(user: string, success: boolean) {}\n"
            )
        },
        bookmark="api-v1",
    ),
    SeedCommit(
        ref="ratelimit",
        parent="api_logging",
        message="Add rate limiting\n\nImplement request throttling for auth endpoints.\n",
        files={"ratelimit.ts": "export class RateLimiter {}\n"},
    ),
    SeedCommit(
        ref="ratelimit_mw",
        parent="ratelimit",
        message=(
            "Add rate limit middleware\n\n"
            "Integrate throttling into API request pipeline.\n"
        ),
        files={"middleware.ts": "export function applyRateLimitMiddleware() {}\n"},
        bookmark="ratelimit-feature",
    ),
    SeedCommit(
        ref="integrate",
        parent="api_logging",
        message=(
            "Integrate all auth modules\n\n"
            "Combine API, database, email, and auth services.\n"
        ),
    ),
    SeedCommit(
        ref="integration_tests",
        parent="integrate",
        message="Add integration tests\n\nTest API endpoints with mocked dependencies.\n",
        files={"tests.ts": "export function testAuthIntegration() {}\n"},
        bookmark="integration-v1",
    ),
    # data-layer stack, off main
    SeedCommit(
        ref="database",
        parent="main",
        message="Add user database layer\n\nImplement persistence for user accounts.\n",
        files={"database.ts": "export class UserRepository {}\n"},
    ),
    SeedCommit(
        ref="queries",
        parent="database",
        message="Add user queries\n\nImplement SELECT queries for user lookups.\n",
        files={
            "database.ts": (
                "export class UserRepository {\n"
                "  async getUserById(id: string) {}\n"
                "}\n"
            )
        },
        bookmark="data-layer",
    ),
    SeedCommit(
        ref="wip",
        parent="queries",
        message="wip: add test file",
        files={"TEST": ""},
    ),
    # cache stack, off main
    SeedCommit(
        ref="cache",
        parent="main",
        message="Implement caching layer\n\nAdd in-memory cache with TTL support.\n",
        files={
            "cache.ts": "export class Cache {}\n",
            "eviction.ts": "export function evictOldEntries() {}\n",
        },
        bookmark="cache-core",
    ),
    SeedCommit(
        ref="eviction",
        parent="cache",
        message="Add cache eviction policy\n\nImplement LRU-based entry removal.\n",
        files={
            "stats.ts": (
                "export function trackCacheHits() {}\n"
                "export function trackCacheMisses() {}\n"
            )
        },
        bookmark="cache-stats",
    ),
    SeedCommit(
        ref="cache_stats",
        parent="eviction",
        message="Add cache statistics tracking\n\nTrack hits, misses, and eviction metrics.\n",
        files={"monitoring.ts": "export function exportMetrics() {}\n"},
    ),
    SeedCommit(
        ref="cache_monitoring",
        parent="cache_stats",
        message=(
            "Add cache monitoring and metrics export\n\n"
            "Integrate with observability systems.\n"
        ),
    ),
    SeedCommit(
        ref="cache_config",
        parent="cache_monitoring",
        message="Add configurable cache parameters\n\nSupport custom max size and TTL settings.\n",
        files={
            "config.ts": (
                "export interface CacheConfig {\n"
                "  maxSize: number;\n"
                "  ttl: number;\n"
                "}\n"
            )
        },
        bookmark="cache-complete",
    ),
]


def run(
    args: list[str],
    *,
    cwd: Path | None = None,
    check: bool = True,
    capture: bool = True,
) -> subprocess.CompletedProcess[str]:
    printable = " ".join(shlex.quote(a) for a in args)
    log(typer.style(f"  $ {printable}", dim=True))
    return subprocess.run(
        args, cwd=cwd, check=check, capture_output=capture, text=True
    )


def jj(playground: Path, *args: str, check: bool = True) -> str:
    result = run(
        ["jj", "--config", "ui.paginate=never", *args], cwd=playground, check=check
    )
    return result.stdout


class Demo:
    """Drives the tmux pane that hosts the asciinema recording."""

    def __init__(self, socket: Path, session: str, cols: int, rows: int):
        self.socket = socket
        self.session = session
        self.cols = cols
        self.rows = rows

    def tmux(self, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
        return run(["tmux", "-S", str(self.socket), *args], check=check)

    def start(self, cwd: Path) -> None:
        self.tmux(
            "new-session",
            "-d",
            "-s",
            self.session,
            "-x",
            str(self.cols),
            "-y",
            str(self.rows),
            "-c",
            str(cwd),
        )

    def kill(self) -> None:
        self.tmux("kill-session", "-t", self.session, check=False)

    def alive(self) -> bool:
        return (
            self.tmux("has-session", "-t", self.session, check=False).returncode == 0
        )

    def pane_text(self) -> str:
        return self.tmux("capture-pane", "-p", "-t", self.session).stdout

    def pane_command(self) -> str:
        return self.tmux(
            "display-message", "-p", "-t", self.session, "#{pane_current_command}"
        ).stdout.strip()

    def send_key(self, key: str) -> None:
        self.tmux("send-keys", "-t", self.session, key)

    def send_literal(self, text: str) -> None:
        self.tmux("send-keys", "-t", self.session, "-l", text)

    def tap(self, key: str, pause: tuple[float, float] = (0.30, 0.60)) -> None:
        """Send one key with a natural pause after it."""
        self.send_key(key)
        time.sleep(random.uniform(*pause))

    def type_text(self, text: str) -> None:
        """Type text character by character with human-ish jitter."""
        for ch in text:
            self.send_literal(ch)
            time.sleep(random.uniform(0.08, 0.14))

    def wait_for(self, needle: str, timeout: float, what: str) -> None:
        """Poll the pane until `needle` appears (literal match)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if needle in self.pane_text():
                return
            time.sleep(0.1)
        raise TimeoutError(
            f"timed out after {timeout}s waiting for {what} "
            f"(needle: {needle!r}); pane content:\n{self.pane_text()}"
        )

    def wait_for_gone(self, needle: str, timeout: float, what: str) -> None:
        """Poll the pane until `needle` no longer appears (literal match)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if needle not in self.pane_text():
                return
            time.sleep(0.1)
        raise TimeoutError(
            f"timed out after {timeout}s waiting for {what} "
            f"(needle still present: {needle!r}); pane content:\n{self.pane_text()}"
        )

    def wait_for_prompt(self, timeout: float) -> None:
        """Wait until the last non-empty pane line is a shell prompt."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            lines = [l for l in self.pane_text().splitlines() if l.strip()]
            if lines and lines[-1].strip().startswith("❯"):
                return
            time.sleep(0.1)
        raise TimeoutError(
            f"timed out after {timeout}s waiting for shell prompt; "
            f"pane content:\n{self.pane_text()}"
        )

    def beat(self, seconds: float) -> None:
        """A deliberate pause, as if reading the screen."""
        time.sleep(seconds)


def build_stakk() -> Path:
    log("Building stakk (release)...")
    result = run(
        [
            "cargo",
            "build",
            "--release",
            "--bin",
            "stakk",
            "--message-format",
            "json",
        ],
        cwd=STAKK_ROOT,
    )
    executable = None
    for line in result.stdout.splitlines():
        msg = json.loads(line)
        if msg.get("executable"):
            executable = msg["executable"]
    if executable is None:
        raise RuntimeError("cargo build produced no executable path")
    return Path(executable)


def repo_slug(playground: Path) -> str:
    remotes = jj(playground, "git", "remote", "list")
    match = re.search(r"github\.com[:/]([^\s]+?)(?:\.git)?\s*$", remotes, re.MULTILINE)
    if match is None:
        raise RuntimeError(f"cannot parse GitHub repo from remotes:\n{remotes}")
    return match.group(1)


def reset_playground(playground: Path) -> None:
    slug = repo_slug(playground)
    log(f"Resetting playground {playground} (remote: {slug})...")

    jj(playground, "git", "fetch")

    # Drop every local bookmark except main, then push the deletions; GitHub
    # auto-closes PRs whose head branch is deleted.
    names = jj(playground, "bookmark", "list", "-T", 'name ++ "\\n"')
    bookmarks = sorted({n for n in names.splitlines() if n and n != "main"})
    if bookmarks:
        jj(playground, "bookmark", "delete", *bookmarks)
    jj(playground, "git", "push", "--deleted", check=False)

    # Sweep remote branches that had no tracked local bookmark.
    result = run(
        ["gh", "api", f"repos/{slug}/branches", "--jq", ".[].name"], check=False
    )
    for branch in result.stdout.splitlines():
        if branch and branch != "main":
            run(
                ["gh", "api", "-X", "DELETE", f"repos/{slug}/git/refs/heads/{branch}"],
                check=False,
            )

    # Close any PR that somehow survived the branch sweep.
    result = run(
        [
            "gh",
            "pr",
            "list",
            "-R",
            slug,
            "--state",
            "open",
            "--json",
            "number",
            "--jq",
            ".[].number",
        ],
        check=False,
    )
    for number in result.stdout.split():
        run(["gh", "pr", "close", "-R", slug, number], check=False)

    # Drop the old graph and reseed.
    jj(playground, "new", "main")
    jj(playground, "abandon", "mutable() ~ @", check=False)


def seed_graph(playground: Path) -> None:
    log("Seeding demo graph...")
    change_ids: dict[str, str] = {}
    for commit in SEED:
        parent = "main" if commit.parent == "main" else change_ids[commit.parent]
        jj(playground, "new", parent)
        for path, content in commit.files.items():
            (playground / path).write_text(content)
        jj(playground, "commit", "-m", commit.message)
        change_id = jj(
            playground, "log", "-r", "@-", "--no-graph", "-T", "change_id"
        ).strip()
        change_ids[commit.ref] = change_id
        if commit.bookmark:
            jj(playground, "bookmark", "create", commit.bookmark, "-r", change_id)
    # Leave the working copy as an empty change on main, like a real session.
    jj(playground, "new", "main")


def record(
    demo: Demo,
    playground: Path,
    stakk_bin: Path,
    cast: Path,
    browse_taps: list[str],
) -> None:
    """Drive the recorded session. Coupled to the select TUI's key bindings
    and screen strings — see the module docstring."""
    with tempfile.TemporaryDirectory(prefix="stakk-demo-zdotdir-") as zdotdir:
        # The recorded zsh sources the user's real config, then shadows
        # `stakk` with the freshly built binary via a shell function
        # (immune to PATH reordering by mise & co).
        (Path(zdotdir) / ".zshrc").write_text(
            "source ~/.zshrc\n" f'stakk() {{ "{stakk_bin}" "$@"; }}\n'
        )

        demo.start(playground)
        log("Recording session...")
        demo.wait_for_prompt(timeout=15)
        demo.send_literal(
            f"SHLVL=0 ZDOTDIR={zdotdir} asciinema record --overwrite {cast}"
        )
        demo.send_key("Enter")
        demo.wait_for(
            "asciinema session started", timeout=10, what="asciinema to start"
        )
        demo.wait_for_prompt(timeout=15)

        # Clear the recorded screen before typing the command. ratatui's
        # inline viewport positions itself with absolute rows from a
        # cursor-position query that is answered by the outer tmux pane,
        # whose cursor sits below the asciinema banner and the
        # pre-recording prompt. `clear` homes both the pane and the cast's
        # virtual screen, so the recorded coordinates match on replay and
        # the TUI opens directly under the prompt instead of leaving a
        # blank gap. The typed `clear` and everything before it are cut
        # from the cast afterwards by `trim_preamble`.
        demo.send_literal("clear")
        demo.send_key("Enter")
        demo.wait_for_gone(
            "asciinema session started", timeout=10, what="the screen to clear"
        )
        demo.wait_for_prompt(timeout=15)
        demo.beat(0.7)

        # Screen 0: type the command.
        demo.type_text("stakk")
        demo.beat(0.4)
        demo.send_key("Enter")

        # Screen 1: browse the branch leaves, settle on the SMTP leaf.
        demo.wait_for("Select branch stack", timeout=15, what="the graph screen")
        demo.beat(1.0)
        for key in browse_taps:
            demo.tap(key, pause=(0.45, 0.75))
        demo.beat(0.8)
        demo.send_key("Enter")

        # Screen 2: assign bookmarks. Cursor starts on the leaf (SMTP) row;
        # every row is [ ] except the pre-existing `auth` bookmark at the
        # bottom. Space cycles [x]→[~]→[>]→[+]→[ ] (states that would
        # produce no or duplicate names are skipped), b cycles backwards.
        demo.wait_for(
            "Assign bookmarks to commits", timeout=15, what="the bookmark screen"
        )
        demo.beat(1.2)
        demo.tap("Space")  # SMTP row: [ ] -> [~] auto (TF-IDF)
        demo.tap("Space")  # -> [>] type
        demo.tap("Space")  # -> [+] new (stakk-…)
        demo.tap("b")  # back to [>]
        demo.tap("b")  # back to [~], the state we keep
        demo.tap("j", pause=(0.5, 0.8))  # down to the templates row
        demo.tap("Space")  # [ ] -> [~]
        demo.tap("r")  # vary the TF-IDF name
        demo.tap("R")  # and back
        demo.tap("j", pause=(0.5, 0.8))  # verification row stays [ ]
        demo.tap("j", pause=(0.5, 0.8))  # down to the password row
        demo.tap("Space")  # [ ] -> [~]
        demo.tap("Space")  # -> [>] type
        demo.tap("i")  # edit the name
        demo.type_text("custom-branch-name")
        demo.beat(0.3)
        demo.send_key("Escape")  # leave edit mode (does not confirm)
        demo.tap("j", pause=(0.5, 0.8))  # logout row stays [ ]
        demo.tap("j", pause=(0.5, 0.8))  # auth row, already [x]
        demo.beat(1.0)
        demo.send_key("Enter")  # confirm — submission starts immediately

        # Real submission: pushes + PR creation against GitHub.
        demo.wait_for(
            "Submitted 4 bookmark(s).", timeout=180, what="the submission to finish"
        )
        demo.beat(1.2)
        demo.send_key("C-d")  # exit the recorded shell, ending the recording

        deadline = time.monotonic() + 15
        while time.monotonic() < deadline and demo.pane_command() == "asciinema":
            time.sleep(0.2)
        time.sleep(0.5)  # let asciinema flush the cast

        for line in demo.pane_text().splitlines():
            if "Created PR" in line or "Existing PR" in line:
                log(line.strip())


def trim_preamble(cast: Path) -> None:
    """Drop cast events that precede the recorded `clear`.

    The choreography runs `clear` before typing the stakk command (see
    `record()`), so the cast opens with the initial prompt and a typed
    `clear` that flash by in the replay. Everything before the
    clear-screen sequence is removed; the replay then starts on the
    cleared screen. The clear event itself is kept — it is a no-op on
    the player's fresh screen. Idempotent: a trimmed cast starts with
    the clear event, so nothing further is dropped.
    """
    lines = cast.read_text().splitlines(keepends=True)
    events = [json.loads(line) for line in lines[1:]]
    start = next(
        (
            i
            for i, ev in enumerate(events)
            if ev[1] == "o" and "\x1b[H\x1b[J" in ev[2]
        ),
        None,
    )
    if start is None:
        raise RuntimeError(
            f"no clear-screen event found in {cast}; expected the "
            "choreography to run `clear` before invoking stakk"
        )
    if start == 0:
        return
    kept = events[start:]
    # Intervals are relative to the previous event, so dropping the preamble
    # already shifts the timeline; zeroing the first interval removes the
    # remaining initial delay.
    kept[0][0] = 0.0
    cast.write_text(
        lines[0]
        + "".join(json.dumps(ev, separators=(",", ":")) + "\n" for ev in kept)
    )
    log(f"Trimmed {start} pre-clear event(s) from cast.")


def inject_theme(cast: Path) -> None:
    lines = cast.read_text().splitlines(keepends=True)
    header = json.loads(lines[0])
    if not header.get("term", {}).get("theme"):
        header.setdefault("term", {})["theme"] = THEME
        lines[0] = json.dumps(header, separators=(",", ":")) + "\n"
        cast.write_text("".join(lines))
        log("Injected terminal theme into cast header.")


def convert(cast: Path, gif: Path, font_family: str) -> None:
    log("Converting cast to gif...")
    gif.unlink(missing_ok=True)
    run(["agg", "--font-family", font_family, str(cast), str(gif)], capture=False)
    log(f"Wrote {gif} ({gif.stat().st_size / 1024:.0f} KiB)")


def main(
    playground: Path = typer.Option(
        STAKK_ROOT.parent / "repo", help="Playground jj repo (will be reset!)."
    ),
    cast: Path = typer.Option(
        STAKK_ROOT / "media" / "stakk.asciinema", help="Output asciicast path."
    ),
    gif: Path = typer.Option(
        STAKK_ROOT / "media" / "stakk.gif", help="Output gif path."
    ),
    cols: int = typer.Option(100, help="Recording terminal width."),
    rows: int = typer.Option(30, help="Recording terminal height."),
    font_family: str = typer.Option(
        "JetBrains Mono,MesloLGS Nerd Font Mono",
        help="agg font families (fallbacks for nerd-font prompt glyphs).",
    ),
    tmux_socket: Path = typer.Option(
        Path(tempfile.gettempdir()) / "stakk-record-demo.sock",
        help="Private tmux socket for the recording session.",
    ),
    keep_session: bool = typer.Option(
        False, help="Keep the tmux session around after a failure, for inspection."
    ),
    skip_reset: bool = typer.Option(
        False, help="Skip the playground reset + reseed phase."
    ),
    skip_record: bool = typer.Option(
        False, help="Skip recording (reuse the existing cast)."
    ),
    skip_convert: bool = typer.Option(False, help="Skip the gif conversion."),
) -> None:
    playground = playground.resolve()
    cast = cast.resolve()
    gif = gif.resolve()

    stakk_bin = build_stakk()
    log(f"Using binary: {stakk_bin}")

    if not skip_reset:
        reset_playground(playground)
        seed_graph(playground)

    if not skip_record:
        demo = Demo(tmux_socket, "stakk-demo", cols, rows)
        # Screen-1 leaf order for the seeded graph: cache, data-layer/wip,
        # integration, ratelimit, email/SMTP (then wraparound). Tour all of
        # them rightward, wrap past the target, and step back onto it.
        browse_taps = ["Right", "Right", "Right", "Right", "Right", "Left"]
        failed = False
        try:
            record(demo, playground, stakk_bin, cast, browse_taps)
        except BaseException:
            failed = True
            raise
        finally:
            if not (failed and keep_session):
                demo.kill()
            elif demo.alive():
                log(
                    f"tmux session kept: tmux -S {tmux_socket} attach -t stakk-demo"
                )

    trim_preamble(cast)
    inject_theme(cast)

    if not skip_convert:
        convert(cast, gif, font_family)


if __name__ == "__main__":
    typer.run(main)
