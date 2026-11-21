#!/usr/bin/env python3
"""Follow a real ejabberd Space and verify fresh-profile restoration in Firefox."""

import base64
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import urllib.parse
from pathlib import Path


def free_port():
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        return reservation.getsockname()[1]


def stop_process(process):
    if process.poll() is None:
        process.terminate()
    try:
        process.wait(timeout=15)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def wait_for_worker(worker, log, port: int):
    expected = f"[wrangler:info] Ready on http://127.0.0.1:{port}".encode()
    deadline = time.monotonic() + 20
    offset = 0
    pending = b""
    while time.monotonic() < deadline:
        if worker.poll() is not None:
            raise RuntimeError("Waddle Worker exited")
        chunk = os.pread(log.fileno(), 8192, offset)
        offset += len(chunk)
        *lines, pending = (pending + chunk).split(b"\n")
        if expected in lines:
            return
        if len(pending) > 8192:
            raise RuntimeError("unexpected Waddle Worker output")
        if not chunk:
            time.sleep(0.25)
    raise RuntimeError("Waddle Worker did not report readiness")


def fixture_ca_der(pem):
    if len(pem) > 16384:
        raise ValueError("fixture CA certificate is too large")
    lines = pem.strip().splitlines()
    if (
        len(lines) < 3
        or lines[0] != "-----BEGIN CERTIFICATE-----"
        or lines[-1] != "-----END CERTIFICATE-----"
    ):
        raise ValueError("expected one fixture CA PEM certificate")
    return base64.b64decode("".join(lines[1:-1]), validate=True)


def fixture_origin(value, scheme, hosts, require_port=False):
    parsed = urllib.parse.urlsplit(value)
    if (
        parsed.scheme != scheme
        or parsed.hostname not in hosts
        or parsed.username is not None
        or parsed.password is not None
        or parsed.path not in ("", "/")
        or parsed.query
        or parsed.fragment
    ):
        raise ValueError("invalid federation browser fixture origin")
    port = parsed.port
    if port == 0 or (require_port and port is None):
        raise ValueError("invalid federation browser fixture port")
    host = parsed.hostname
    origin = f"{scheme}://{host}" if port is None else f"{scheme}://{host}:{port}"
    return origin, host, port


def run_profile(config, service, node, room, restore):
    chat = Path(__file__).resolve().parents[3] / "chat"
    home, home_domain, _ = fixture_origin(
        config["home"], "https", {"home-a.test", "home-b.test"}
    )
    firefox_local_host = "home-a.test" if home_domain == "home-a.test" else "home-b.test"
    local_hosts = {"127.0.0.1", "localhost"}
    issuer, issuer_host, issuer_port = fixture_origin(
        config["issuer"], "http", local_hosts, require_port=True
    )
    issuer_cookie = config["issuer_cookie"]
    ca = fixture_ca_der(config["ca_pem"])
    origin, _, origin_port = fixture_origin(
        config["chat_origin"], "http", local_hosts, require_port=True
    )
    built_config = chat / "dist/server/wrangler.json"
    worker_config = json.loads(built_config.read_text())
    worker_config.setdefault("vars", {})["SERVER_BASE_URL"] = home
    worker_config.setdefault("dev", {}).update(ip="127.0.0.1", port=origin_port)
    # Wrangler resolves build output and asset paths relative to its config.
    with (
        tempfile.TemporaryDirectory(prefix="waddle-firefox-") as profile,
        tempfile.TemporaryFile() as worker_log,
        tempfile.NamedTemporaryFile(
            mode="w", dir=built_config.parent, prefix="interop-", suffix=".json"
        ) as local_config,
    ):
        json.dump(worker_config, local_config)
        local_config.flush()
        worker = subprocess.Popen(
            [
                "node",
                "node_modules/.bin/wrangler",
                "dev",
                "--config",
                local_config.name,
                "--local",
                "--show-interactive-dev-session=false",
            ],
            cwd=chat,
            env={**os.environ, "WRANGLER_SEND_METRICS": "false"},
            stdout=worker_log,
            stderr=subprocess.STDOUT,
        )
        try:
            wait_for_worker(worker, worker_log, origin_port)
            port = free_port()
            Path(profile, "user.js").write_text(
                f'user_pref("marionette.port", {port});\n'
                f'user_pref("network.dns.localDomains", "{firefox_local_host}");\n'
                'user_pref("network.trr.mode", 5);\n'
                'user_pref("network.proxy.type", 0);\n'
            )
            subprocess.run(
                ["certutil", "-N", "--empty-password", "-d", f"sql:{profile}"], check=True
            )
            subprocess.run(
                [
                    "certutil",
                    "-A",
                    "-d",
                    f"sql:{profile}",
                    "-n",
                    "Waddle fixture CA",
                    "-t",
                    "C,,",
                ],
                input=ca,
                check=True,
            )
            browser = subprocess.Popen(
                [
                    "firefox",
                    "--headless",
                    "--no-remote",
                    "--profile",
                    profile,
                    "--marionette",
                ],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            try:
                for _ in range(50):
                    try:
                        connection = socket.create_connection(
                            ("127.0.0.1", port), timeout=5
                        )
                        break
                    except OSError:
                        time.sleep(0.2)
                else:
                    raise RuntimeError("Firefox did not start Marionette")
                stream = connection.makefile("rb")

                def receive():
                    prefix = bytearray()
                    while (byte := stream.read(1)) != b":":
                        if not byte:
                            raise EOFError()
                        prefix.extend(byte)
                    return json.loads(stream.read(int(prefix)))

                def call(number, method, arguments):
                    encoded = json.dumps([0, number, method, arguments]).encode()
                    connection.sendall(str(len(encoded)).encode() + b":" + encoded)
                    response = receive()
                    assert response[:3] == [1, number, None], response
                    return response[3]

                assert receive()["applicationType"] == "gecko"
                call(1, "WebDriver:NewSession", {"capabilities": {}})
                call(2, "WebDriver:Navigate", {"url": issuer + "/sign-in"})
                current = call(3, "WebDriver:GetCurrentURL", {})["value"]
                location = urllib.parse.urlsplit(current)
                if (
                    location.scheme != "http"
                    or location.hostname != issuer_host
                    or (location.port or 80) != issuer_port
                    or location.username is not None
                    or location.password is not None
                ):
                    raise RuntimeError("disposable Colony sign-in left the fixture origin")
                call(
                    4,
                    "WebDriver:AddCookie",
                    {
                        "cookie": {
                            "name": "better-auth.session_token",
                            "value": issuer_cookie,
                            "path": "/",
                            "httpOnly": True,
                        }
                    },
                )
                call(5, "WebDriver:Navigate", {"url": origin + "/rooms"})
                command = 5

                def execute(script, args=None):
                    nonlocal command
                    command += 1
                    return call(
                        command,
                        "WebDriver:ExecuteScript",
                        {"script": script, "args": args or []},
                    )["value"]

                def wait(script, args=None):
                    for attempt in range(250):
                        answer = execute(script, args)
                        if answer:
                            return answer
                        time.sleep(0.2)
                    raise AssertionError(
                        execute("return document.body.innerText.slice(0, 2500)")
                    )

                def press(label, scope="body"):
                    wait(
                        """const [label, scope] = arguments;
                        const target = [...(document.querySelector(scope)?.querySelectorAll('button') ?? [])]
                            .find(button => button.textContent.trim() === label && !button.disabled);
                        if (target) { target.click(); return true; }""",
                        [label, scope],
                    )

                press("Sign in with Local Colony")
                wait(
                    """if (location.origin === arguments[0]) {
                            const approve = [...document.querySelectorAll('button')]
                                .find(button => button.textContent.trim() === 'Approve');
                            if (approve && !approve.disabled) approve.click();
                            return false;
                        }
                        return [...document.querySelectorAll('button')]
                            .some(button => button.textContent.trim() === 'Join from another server');""",
                    [issuer],
                )
                press("Join from another server")
                print(
                    "PASS: Firefox authenticated to Waddle with the disposable Colony account."
                )

                def followed_space():
                    wait(
                        """const [service, room] = arguments;
                        return [...document.querySelectorAll('[aria-label="Your Spaces on other servers"] article')]
                            .some(row => row.innerText.includes(service + ' · Following') && row.innerText.includes(room));""",
                        [service, room],
                    )

                def joined_room():
                    wait(
                        """const [room] = arguments;
                        return [...document.querySelectorAll('[aria-label="Your rooms on other servers"] article')]
                            .some(row => row.innerText.includes(room) && row.innerText.includes('· Joined'));""",
                        [room],
                    )

                lookup = '[aria-label="Lookup result"]'
                if restore:
                    followed_space()
                    joined_room()
                    print(
                        "PASS: fresh Firefox profile restored the followed Space and joined room."
                    )
                else:
                    wait(
                        """const input = document.querySelector('#remote-community-address');
                        if (input) {
                            input.value = arguments[0];
                            input.dispatchEvent(new Event('input', {bubbles: true}));
                            input.form.requestSubmit();
                            return true;
                        }""",
                        [f"xmpp:{service}?;node={urllib.parse.quote(node, safe='')}"],
                    )
                    wait(
                        """const [room] = arguments;
                        return [...document.querySelectorAll('[aria-label="Lookup result"] li')]
                            .some(row => row.innerText.includes(room));""",
                        [room],
                    )
                    press("Follow", lookup)
                    followed_space()
                    wait(
                        """const [room] = arguments;
                        const row = [...document.querySelectorAll('[aria-label="Lookup result"] li')]
                            .find(item => item.innerText.includes(room));
                        const button = row?.querySelector('button');
                        if (button && !button.disabled) { button.click(); return true; }""",
                        [room],
                    )
                    press("Join", lookup)
                    wait(
                        """return !document.querySelector('#remote-community-address')"""
                    )
                    wait(
                        """return [...document.querySelectorAll('[role="status"] h2')]
                            .some(heading => heading.textContent.trim() === "It's quiet in #" + arguments[0]);""",
                        [room],
                    )
                    command += 1
                    call(command, "WebDriver:Navigate", {"url": origin + "/rooms"})
                    press("Join from another server")
                    followed_space()
                    joined_room()
                    print(
                        "PASS: Firefox followed the chord-created ejabberd Space and joined its listed room."
                    )
            finally:
                stop_process(browser)

        finally:
            stop_process(worker)


if __name__ == "__main__":
    fixture = json.loads(Path(os.environ["INTEROP_CHORD_BROWSER_CONFIG"]).read_text())
    addresses = sys.argv[1:]
    for restore in (False, True):
        run_profile(fixture, *addresses, restore=restore)
