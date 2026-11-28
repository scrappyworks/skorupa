{ pkgs, compositor }:

pkgs.testers.runNixOSTest {
  name = "skorupa-session";
  globalTimeout = 120;
  extraPythonPackages = packages: [ packages.pillow ];

  nodes.machine = { pkgs, ... }: {
    users.users.alice = { isNormalUser = true; uid = 1000; };
    services.getty.autologinUser = "alice";
    hardware.graphics.enable = true;
    fonts.packages = [ pkgs.dejavu_fonts ];
    environment.systemPackages = [ compositor pkgs.foot pkgs.wayland-utils ];
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
        exec systemd-cat --identifier=skorupa ${compositor}/bin/skorupa-compositor --drm
      fi
    '';
  };

  testScript = builtins.readFile ./session.py;
}
