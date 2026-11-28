"""REQ-LOCK-002/003/014/019 and REQ-AUTH-001/002/003/017 on the guest KMS seat."""

from PIL import Image

machine.start()
machine.wait_for_unit("user@1000.service")
runtime = "/run/user/1000"
control = "/run/skorupa-lock-vm"
user = f"runuser -u alice -- env XDG_RUNTIME_DIR={runtime}"


def wait_marker(name: str) -> None:
    machine.wait_for_file(f"{control}/{name}")


def signal(name: str) -> None:
    machine.succeed(f"touch {control}/{name}")


def start_client(number: int) -> None:
    name = f"client-{number}"
    machine.succeed(
        f"{user} systemd-run --user --unit={name} -E WAYLAND_DISPLAY=wayland-1 "
        f"skorupa-vm-client {number} {control}/{name}.input {control}/{name}.mapped"
    )
    wait_marker(f"{name}.mapped")


def verify_frame(name: str, covered: bool) -> None:
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
                expected = (197, 42, 55)
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
    wait_marker("challenge-1")
    signal("answer-1")
    wait_marker("invalid-dispatched-1")
    signal("deny-1")
    # Only a current denial can retire the first attempt and permit another one.
    wait_marker("challenge-2")
    verify_frame("startup-denied", covered=True)
    signal("answer-2")
    verify_frame("startup-unlocked", covered=False)
    machine.send_key("b")
    wait_marker("client-1.input")
    # The earlier A would precede B on the same Wayland connection.
    assert machine.succeed(f"cat {control}/client-1.input") == "b"

with subtest("relocking fences a mapped client and a new surface"):
    signal("lock")
    wait_marker("challenge-3")
    verify_frame("relocked", covered=True)
    start_client(2)
    verify_frame("relocked-after-client-commit", covered=True)
    machine.send_key("a")

with subtest("replayed verdict stays locked and a current verdict restores focus"):
    signal("answer-3")
    wait_marker("invalid-dispatched-3")
    signal("deny-3")
    wait_marker("challenge-4")
    verify_frame("replay-refused", covered=True)
    signal("answer-4")
    verify_frame("unlocked-again", covered=False)
    machine.send_key("b")
    machine.wait_until_succeeds(f"test \"$(cat {control}/client-1.input)\" != b")
    assert machine.succeed(f"cat {control}/client-1.input") == "bb"
    machine.fail(f"test -e {control}/client-2.input")

with subtest("broker disconnect restores the cover"):
    signal("close-broker")
    wait_marker("disconnect-covered")
    verify_frame("broker-disconnected", covered=True)
