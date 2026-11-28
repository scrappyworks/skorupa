"""REQ-LAYOUT-003/006 and REQ-INPUT-002 on a real guest seat and KMS output."""

from PIL import Image

machine.start()
machine.wait_for_unit("user@1000.service")
runtime_dir = "/run/user/1000"
user = f"runuser -u alice -- env XDG_RUNTIME_DIR={runtime_dir}"
wayland = f"{user} WAYLAND_DISPLAY=wayland-1"
machine.wait_for_file(f"{runtime_dir}/wayland-1")
machine.succeed(f"{wayland} wayland-info")


def start_client(number: int) -> None:
    name = f"client-{number}"
    machine.succeed(
        f"{user} systemd-run --user --unit={name} -E WAYLAND_DISPLAY=wayland-1 "
        f"skorupa-vm-client {number} {runtime_dir}/{name}.input"
    )
    machine.wait_until_succeeds(f"{user} systemctl --user is-active --quiet {name}")


def verify_screen(name: str, color: tuple[int, int, int]) -> None:
    pixel = None
    for _ in range(12):
        machine.screenshot(name)
        with Image.open(machine.out_dir / (name + ".png")) as image:
            pixel = image.convert("RGB").getpixel((image.width // 4, image.height // 2))
        if pixel == color:
            return
        machine.succeed("sleep 0.25")
    raise AssertionError(f"{name}: expected {color} at the sample point, saw {pixel}")


start_client(1)
verify_screen("first-workspace", (197, 42, 55))
start_client(2)
verify_screen("first-remains-visible", (197, 42, 55))

machine.send_key("meta_l-2")
verify_screen("second-workspace", (41, 63, 188))
machine.send_key("a")
machine.wait_until_succeeds(f"test -f {runtime_dir}/client-2.input")
assert machine.succeed(f"cat {runtime_dir}/client-2.input") == "a"
machine.fail(f"test -e {runtime_dir}/client-1.input")
machine.send_key("meta_l-1")
verify_screen("returned-to-first", (197, 42, 55))
