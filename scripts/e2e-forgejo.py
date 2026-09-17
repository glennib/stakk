#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["typer"]
# ///
"""Lifecycle of the ephemeral Forgejo instance behind the e2e suite.

`up` starts a Forgejo container, waits for it to answer, creates the admin
user and an API token, and writes `.e2e-forgejo.env` at the repo root.
`env` prints that file as `export` lines, `test` runs the e2e nextest
profile with it applied, `logs` dumps the container log, and `down`
removes the container and the file. The same script runs locally and in
CI.

The container has no volume and is never `--rm`: `logs` has to work after
a failed run, so the container outlives its process until `down`.
"""

from __future__ import annotations

import os
import shlex
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from enum import Enum
from pathlib import Path

import typer

# Exact tag, bumped by Renovate through the regex manager in renovate.json.
# The rootless variant is used deliberately: on it a plain
# `<runtime> exec <container> forgejo admin ...` works, with no user or
# config-path flags to keep in step with the image.
FORGEJO_IMAGE = "codeberg.org/forgejo/forgejo:16.0.5-rootless"

CONTAINER = "stakk-e2e-forgejo"
CONTAINER_PORT = 3000
DEFAULT_HOST_PORT = 3000
HEALTH_TIMEOUT_SECONDS = 60.0

USER = "stakk"
# Fixed and printed by `up`, so the web UI is usable for poking at what a
# test left behind. The instance is local and ephemeral; there is nothing
# to protect.
PASSWORD = "stakk-e2e-password"
EMAIL = "stakk@example.invalid"

STAKK_ROOT = Path(__file__).resolve().parents[1]
ENV_FILE = STAKK_ROOT / ".e2e-forgejo.env"

# The contract with the tests: exactly these three keys, in this order.
ENV_URL = "STAKK_E2E_FORGEJO_URL"
ENV_USER = "STAKK_E2E_FORGEJO_USER"
ENV_TOKEN = "STAKK_E2E_FORGEJO_TOKEN"

SCRIPT = f"scripts/{Path(__file__).name}"


def log(message: str = "") -> None:
    """Progress reporting goes to stderr; stdout stays clean."""
    typer.echo(message, err=True)


class E2eError(Exception):
    """A failure with a message meant for the user, not a traceback."""


def run(
    args: list[str],
    *,
    check: bool = True,
    capture: bool = True,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    printable = " ".join(shlex.quote(a) for a in args)
    log(typer.style(f"  $ {printable}", dim=True))
    return subprocess.run(
        args,
        cwd=STAKK_ROOT,
        check=check,
        capture_output=capture,
        text=True,
        env=env,
    )


class ContainerState(Enum):
    MISSING = "missing"
    STOPPED = "stopped"
    RUNNING = "running"


@dataclass
class Runtime:
    """The container runtime CLI (podman or docker) and the one container."""

    binary: str

    @staticmethod
    def detect() -> Runtime:
        override = os.environ.get("STAKK_E2E_RUNTIME")
        if override:
            if shutil.which(override) is None:
                raise E2eError(f"STAKK_E2E_RUNTIME={override!r} is not on PATH")
            return Runtime(override)
        for candidate in ("podman", "docker"):
            if shutil.which(candidate) is not None:
                return Runtime(candidate)
        raise E2eError(
            "neither podman nor docker is on PATH "
            "(set STAKK_E2E_RUNTIME to name the runtime binary)"
        )

    def state(self) -> ContainerState:
        result = run(
            [self.binary, "container", "inspect", "-f", "{{.State.Running}}", CONTAINER],
            check=False,
        )
        if result.returncode != 0:
            return ContainerState.MISSING
        if result.stdout.strip() == "true":
            return ContainerState.RUNNING
        return ContainerState.STOPPED

    def host_port(self) -> int:
        """The host port an existing container publishes Forgejo on."""
        # `HostConfig.PortBindings` is recorded at `run` time, so unlike
        # `<runtime> port` it answers for a stopped container too.
        template = (
            f'{{{{(index (index .HostConfig.PortBindings "{CONTAINER_PORT}/tcp") 0)'
            ".HostPort}}"
        )
        result = run([self.binary, "container", "inspect", "-f", template, CONTAINER])
        value = result.stdout.strip()
        if not value.isdigit():
            raise E2eError(
                f"cannot read the published port of {CONTAINER} "
                f"(inspect printed {value!r}); remove it with `{SCRIPT} down`"
            )
        return int(value)

    def create(self, port: int) -> None:
        settings = {
            "security__INSTALL_LOCK": "true",
            "server__ROOT_URL": f"http://localhost:{port}/",
            "database__DB_TYPE": "sqlite3",
            "service__DISABLE_REGISTRATION": "true",
            "mailer__ENABLED": "false",
            "log__LEVEL": "Warn",
        }
        args = [self.binary, "run", "-d", "--name", CONTAINER]
        args += ["-p", f"{port}:{CONTAINER_PORT}"]
        for key, value in settings.items():
            args += ["-e", f"FORGEJO__{key}={value}"]
        args.append(FORGEJO_IMAGE)
        run(args)

    def start(self) -> None:
        run([self.binary, "start", CONTAINER])

    def exec(self, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
        return run([self.binary, "exec", CONTAINER, *args], check=check)

    def remove(self) -> None:
        result = run([self.binary, "rm", "-f", CONTAINER], check=False)
        if result.returncode != 0 and "no such container" not in result.stderr.lower():
            raise E2eError(
                f"{self.binary} rm -f {CONTAINER} failed:\n{result.stderr.strip()}"
            )

    def logs(self) -> int:
        """Stream the container log to stdout (both of its streams merged)."""
        result = subprocess.run(
            [self.binary, "logs", CONTAINER], stderr=subprocess.STDOUT, check=False
        )
        return result.returncode


def wait_for_health(url: str) -> None:
    endpoint = f"{url}/api/v1/version"
    log(f"Waiting for {endpoint} ...")
    deadline = time.monotonic() + HEALTH_TIMEOUT_SECONDS
    last_error = "no response yet"
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(endpoint, timeout=5) as response:
                if response.status == 200:
                    return
                last_error = f"HTTP {response.status}"
        except urllib.error.HTTPError as error:
            last_error = f"HTTP {error.code}"
        except (urllib.error.URLError, OSError) as error:
            last_error = str(error.reason if hasattr(error, "reason") else error)
        time.sleep(0.5)
    raise E2eError(
        f"Forgejo did not answer 200 on {endpoint} within "
        f"{HEALTH_TIMEOUT_SECONDS:.0f}s (last error: {last_error}); "
        f"inspect the container with `{SCRIPT} logs`"
    )


def ensure_admin_user(runtime: Runtime) -> bool:
    """Create the admin user; returns whether this call created it."""
    result = runtime.exec(
        "forgejo",
        "admin",
        "user",
        "create",
        "--admin",
        "--username",
        USER,
        "--password",
        PASSWORD,
        "--email",
        EMAIL,
        "--must-change-password=false",
        check=False,
    )
    if result.returncode == 0:
        return True
    output = result.stdout + result.stderr
    if "already exists" in output.lower():
        return False
    raise E2eError(f"creating the admin user failed:\n{output.strip()}")


def generate_token(runtime: Runtime, name: str) -> str:
    result = runtime.exec(
        "forgejo",
        "admin",
        "user",
        "generate-access-token",
        "--username",
        USER,
        "--token-name",
        name,
        "--scopes",
        "all",
        "--raw",
    )
    token = result.stdout.strip()
    if not token:
        raise E2eError(
            f"generate-access-token printed no token; stderr:\n{result.stderr.strip()}"
        )
    return token


def write_env_file(url: str, token: str) -> None:
    ENV_FILE.write_text(
        f"{ENV_URL}={url}\n{ENV_USER}={USER}\n{ENV_TOKEN}={token}\n"
    )


def read_env_file() -> dict[str, str]:
    if not ENV_FILE.exists():
        raise E2eError(f"{ENV_FILE} does not exist; run `{SCRIPT} up` first")
    variables: dict[str, str] = {}
    for line in ENV_FILE.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        key, _, value = line.partition("=")
        variables[key] = value
    missing = [k for k in (ENV_URL, ENV_USER, ENV_TOKEN) if k not in variables]
    if missing:
        raise E2eError(
            f"{ENV_FILE} lacks {', '.join(missing)}; "
            f"delete it and run `{SCRIPT} up` again"
        )
    return variables


def bring_up(runtime: Runtime, port: int) -> dict[str, str]:
    """Idempotent `up`; returns the env file's variables."""
    state = runtime.state()
    if state is ContainerState.MISSING:
        log(f"Starting {CONTAINER} from {FORGEJO_IMAGE} on port {port} ...")
        runtime.create(port)
    else:
        actual = runtime.host_port()
        if actual != port:
            log(f"{CONTAINER} exists and publishes port {actual}; using that.")
            port = actual
        if state is ContainerState.STOPPED:
            log(f"Starting the stopped {CONTAINER} ...")
            runtime.start()
        elif ENV_FILE.exists():
            log(f"{CONTAINER} is running and {ENV_FILE} exists; nothing to do.")
            return read_env_file()
        else:
            log(f"{CONTAINER} is running but {ENV_FILE} is missing; re-bootstrapping.")

    url = f"http://localhost:{port}"
    wait_for_health(url)

    created = ensure_admin_user(runtime)
    # Token names are unique per user, and an existing token cannot be read
    # back, so a re-bootstrap mints a new one under a fresh name.
    token_name = "e2e" if created else f"e2e-{int(time.time())}"
    log(f"Generating API token {token_name!r} for {USER} ...")
    token = generate_token(runtime, token_name)
    write_env_file(url, token)
    log(f"Wrote {ENV_FILE}")
    return read_env_file()


def is_ready(runtime: Runtime) -> bool:
    return runtime.state() is ContainerState.RUNNING and ENV_FILE.exists()


app = typer.Typer(
    help=__doc__,
    no_args_is_help=True,
    add_completion=False,
    context_settings={"help_option_names": ["-h", "--help"]},
)

PORT_OPTION = typer.Option(
    DEFAULT_HOST_PORT,
    help="Host port to publish Forgejo on (ignored when the container already exists).",
)


@app.command()
def up(port: int = PORT_OPTION) -> None:
    """Start Forgejo, bootstrap the admin user and token, write the env file."""
    runtime = Runtime.detect()
    variables = bring_up(runtime, port)
    log()
    log(f"Forgejo:  {variables[ENV_URL]}")
    log(f"User:     {USER}")
    log(f"Password: {PASSWORD}")
    log(f"Env file: {ENV_FILE}")
    log(f'Export:   eval "$({SCRIPT} env)"')


@app.command()
def down() -> None:
    """Remove the container and the env file."""
    runtime = Runtime.detect()
    runtime.remove()
    ENV_FILE.unlink(missing_ok=True)
    log(f"Removed {CONTAINER} and {ENV_FILE}")


@app.command()
def env(
    github: bool = typer.Option(
        False, "--github", help="Append KEY=VALUE lines to $GITHUB_ENV instead."
    ),
) -> None:
    """Print the env file as `export` lines for `eval "$(... env)"`."""
    variables = read_env_file()
    if github:
        target = os.environ.get("GITHUB_ENV")
        if not target:
            raise E2eError("--github requires $GITHUB_ENV to be set")
        with open(target, "a", encoding="utf-8") as handle:
            for key, value in variables.items():
                handle.write(f"{key}={value}\n")
        log(f"Appended {', '.join(variables)} to {target}")
        return
    for key, value in variables.items():
        typer.echo(f"export {key}={shlex.quote(value)}")


@app.command(
    context_settings={"allow_extra_args": True, "ignore_unknown_options": True}
)
def test(ctx: typer.Context, port: int = PORT_OPTION) -> None:
    """Run `cargo nextest run --profile e2e`; extra arguments go to nextest."""
    runtime = Runtime.detect()
    if is_ready(runtime):
        variables = read_env_file()
    else:
        variables = bring_up(runtime, port)
    result = run(
        ["cargo", "nextest", "run", "--profile", "e2e", *ctx.args],
        check=False,
        capture=False,
        env={**os.environ, **variables},
    )
    raise typer.Exit(result.returncode)


@app.command()
def logs() -> None:
    """Dump the container log to stdout."""
    raise typer.Exit(Runtime.detect().logs())


if __name__ == "__main__":
    try:
        app()
    except E2eError as error:
        log(typer.style(f"error: {error}", fg="red", bold=True))
        sys.exit(1)
