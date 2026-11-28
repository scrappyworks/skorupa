{ pkgs, compositor, lockTest ? false }:

let
  control = "/run/skorupa-lock-vm";
  command =
    if lockTest then
      "${compositor}/bin/skorupa-lock-vm seat ${control}"
    else
      "${compositor}/bin/skorupa-compositor --drm";
in

pkgs.testers.runNixOSTest {
  name = if lockTest then "skorupa-lock-session" else "skorupa-session";
  globalTimeout = 120;
  extraPythonPackages = packages: [ packages.pillow ];

  nodes.machine = { pkgs, ... }: {
    users.users.alice = { isNormalUser = true; uid = 1000; };
    services.getty.autologinUser = "alice";
    hardware.graphics.enable = true;
    systemd.tmpfiles.rules = pkgs.lib.optional lockTest "d ${control} 0700 alice users -";
    fonts.packages = [ pkgs.dejavu_fonts ];
    environment.systemPackages = [ compositor pkgs.wayland-utils ];
    virtualisation = {
      cores = 2;
      memorySize = 2048;
      qemu.options = [ "-vga" "none" "-device" "virtio-gpu-pci" ];
    };

    # The compositor belongs to the tty's real logind session. The user
    # needs neither the video group nor an input device grant on the host.
    programs.bash.loginShellInit = ''
      if [ "$(tty)" = /dev/tty1 ]; then
        export LIBSEAT_BACKEND=logind LIBGL_ALWAYS_SOFTWARE=1
        export LD_LIBRARY_PATH=${pkgs.libglvnd}/lib:/run/opengl-driver/lib
        exec systemd-cat --identifier=skorupa ${command}
      fi
    '';
  };

  testScript = builtins.readFile (if lockTest then ./lock-session.py else ./session.py);
}
