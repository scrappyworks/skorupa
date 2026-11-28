"""REQ-LOCK-002/003/011/014/019 and REQ-AUTH-001/002/003/017 on the guest KMS seat."""

from datetime import timedelta

from PIL import Image

machine.start()
machine.wait_for_unit("user@1000.service")
runtime = "/run/user/1000"
control = "/run/skorupa-lock-vm"
user = f"runuser -u alice -- env XDG_RUNTIME_DIR={runtime}"


def wait_marker(name: str) -> None:
    machine.wait_for_file(f"{control}/{name}")


def wait_challenge(number: int) -> None:
    # A fresh challenge after denial must arrive before the 30 s attempt expires.
    machine.wait_for_file(f"{control}/challenge-{number}", timeout=timedelta(seconds=10))


def signal(name: str) -> None:
    machine.succeed(f"touch {control}/{name}")


def start_client(number: int) -> None:
    name = f"client-{number}"
    machine.succeed(
        f"{user} systemd-run --user --unit={name} -E WAYLAND_DISPLAY=wayland-1 "
        f"skorupa-vm-client {number} {control}/{name}.input {control}/{name}.mapped"
    )
    wait_marker(f"{name}.mapped")


def verify_frame(name: str, covered: bool, workspace: int = 1) -> None:
    observed = None
    for _ in range(16):
        machine.screenshot(name)
        with Image.open(machine.out_dir / (name + ".png")) as image:
            rgb = image.convert("RGB")
            if covered:
                observed = rgb.getextrema()
                expected = ((0, 0), (0, 0), (0, 0))
            else:
                observed = rgb.getpixel((image.width // 4, image.height // 2))
                expected = (197, 42, 55) if workspace == 1 else (41, 63, 188)
        if observed == expected:
            return
        machine.succeed("sleep 0.25")
    raise AssertionError(f"{name}: unexpected frame sample {observed}")


with subtest("startup holds input and presents only the cover"):
    machine.wait_until_succeeds(f"test -S {runtime}/wayland-1")
    wait_marker("startup-covered")
    start_client(1)
    verify_frame("startup-covered", covered=True)
    machine.fail(f"test -e {control}/challenge-1")
    machine.send_key("a")

with subtest("stale grant and current denial keep startup locked"):
    signal("bridge-awake")
    wait_challenge(1)
    signal("answer-1")
    wait_challenge(2)
    verify_frame("startup-denied", covered=True)
    signal("answer-2")
    verify_frame("startup-unlocked", covered=False)
    machine.send_key("b")
    wait_marker("client-1.input")
    # The earlier A would precede B on the same Wayland connection.
    assert machine.succeed(f"cat {control}/client-1.input") == "b"

with subtest("relocking fences a mapped client and a new surface"):
    signal("lock")
    wait_challenge(3)
    verify_frame("relocked", covered=True)
    start_client(2)
    verify_frame("relocked-after-client-commit", covered=True)
    machine.send_key("a")

with subtest("replayed verdict stays locked and a current verdict restores focus"):
    signal("answer-3")
    wait_challenge(4)
    verify_frame("replay-refused", covered=True)
    signal("answer-4")
    verify_frame("unlocked-again", covered=False)
    machine.send_key("b")
    machine.wait_until_succeeds(f"test \"$(cat {control}/client-1.input)\" != b")
    assert machine.succeed(f"cat {control}/client-1.input") == "bb"
    machine.fail(f"test -e {control}/client-2.input")

with subtest("ordinary workspaces redraw while a previous KMS completion is withheld"):
    signal("hold-session-flip")
    verify_frame("held-flip-on-second", covered=False, workspace=2)
    signal("return-to-first")
    verify_frame("held-flip-back-on-first", covered=False)
    signal("release-session-flip")

with subtest("missing KMS completion turns the output off within the lock budget"):
    signal("delay-kms")
    machine.wait_for_file(f"{control}/fallback-dark", timeout=timedelta(seconds=5))
    assert machine.succeed("cat /sys/class/drm/card*-Virtual-*/enabled").strip() == "disabled"
    machine.fail(f"test -e {control}/challenge-5")
    signal("resume-kms")
    wait_challenge(5)
    verify_frame("fallback-woke-covered", covered=True)
    signal("answer-5")
    verify_frame("fallback-unlocked", covered=False)

with subtest("seat loss invalidates an open verdict before the next cover"):
    signal("vt-lock")
    wait_challenge(6)
    signal("vt-switch")
    machine.succeed("chvt 2")
    wait_marker("vt-fenced")
    signal("answer-6")
    machine.fail(f"test -e {control}/challenge-7")
    machine.succeed("chvt 1")
    wait_challenge(7)
    verify_frame("seat-return-covered", covered=True)
    signal("answer-7")
    verify_frame("seat-return-unlocked", covered=False)

with subtest("broker disconnect restores the cover"):
    signal("close-broker")
    wait_marker("disconnect-covered")
    verify_frame("broker-disconnected", covered=True)
