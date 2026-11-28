{ pkgs }:

let
  python = pkgs.python3.withPackages (p: [ p.dbus-python p.pygobject3 ]);
  probe = pkgs.writeShellScriptBin "skorupa-logind-probe" ''
    export GI_TYPELIB_PATH=${pkgs.glib.out}/lib/girepository-1.0
    exec ${python}/bin/python ${./logind-probe.py} "$@"
  '';
in
pkgs.testers.runNixOSTest {
  name = "skorupa-logind-probe";
  globalTimeout = 180;
  nodes.machine = { lib, pkgs, ... }: {
    users.users.probe = { isNormalUser = true; uid = 1000; linger = true; };
    systemd.tmpfiles.rules = [ "d /run/probe 0750 probe users -" ];
    security.polkit.enable = true;
    security.polkit.extraConfig = ''
      polkit.addRule(function (action, subject) {
        if (action.id == "org.freedesktop.login1.inhibit-delay-sleep" && subject.user == "probe")
          return polkit.Result.YES;
      });
    '';
    services.logind.settings.Login.InhibitDelayMaxSec = 8;
    environment.systemPackages = [ probe ];
    systemd.user.services.hold = {
      unitConfig.ConditionUser = "probe";
      serviceConfig = {
        ExecStart = "${probe}/bin/skorupa-logind-probe worker";
        NotifyAccess = "main";
        FileDescriptorStoreMax = 1;
        Restart = "always";
        RestartSec = "250ms";
      };
    };
    # Exercise logind's real asynchronous suspend job without sleeping the VM.
    systemd.services.systemd-suspend.serviceConfig.ExecStart = lib.mkForce
      [ "" "${pkgs.runtimeShell} -c 'touch /run/probe/suspend; test ! -e /run/probe/fail'" ];
  };
  testScript = builtins.readFile ./logind.py;
}
