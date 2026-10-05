# The NixOS module's service, in VMs without a Bluetooth adapter. `machine`
# runs it as the dynamic user, then as root with remote logins, as root
# beside CUPS, and as the dynamic user again, keeping its printers
# throughout; `immutable` runs it as root with an immutable /etc.
{
  name = "phomemo-printer-app";

  nodes.machine =
    { pkgs, ... }:
    {
      services.phomemo-printer-app.enable = true;

      # BlueZ starts once the kernel's Bluetooth support is loaded, and runs
      # without an adapter.
      boot.kernelModules = [ "bluetooth" ];

      environment.systemPackages = [ pkgs.curl ];

      users.users = {
        alice = {
          isNormalUser = true;
          password = "alice-password";
          extraGroups = [ "wheel" ];
        };
        bob = {
          isNormalUser = true;
          password = "bob-password";
        };
      };

      specialisation.root.configuration = {
        services.phomemo-printer-app = {
          runAsRoot = true;
          listenHostname = "*";
          port = 631;
          openFirewall = true;
          adminGroup = "wheel";
          tlsOnly = true;
        };
        hardware.bluetooth.enable = false;
      };

      # On every address, but behind the firewall.
      specialisation.cups.configuration = {
        services.phomemo-printer-app = {
          runAsRoot = true;
          listenHostname = "*";
        };
        services.printing.enable = true;
      };

      specialisation.lowport.configuration = {
        services.phomemo-printer-app = {
          port = 631;
          logFile = "/var/lib/phomemo-printer-app/server.log";
        };
      };
    };

  nodes.immutable =
    { pkgs, ... }:
    {
      services.phomemo-printer-app = {
        enable = true;
        runAsRoot = true;
      };

      system.etc.overlay = {
        enable = true;
        mutable = false;
      };
      # What an immutable /etc needs.
      systemd.sysusers.enable = true;
      users.mutableUsers = false;
      boot.initrd.systemd.enable = true;
      time.timeZone = "UTC";
      services.resolved.enable = true;

      environment.systemPackages = [ pkgs.curl ];
    };

  nodes.client =
    { pkgs, ... }:
    {
      environment.systemPackages = [
        pkgs.cups # ipptool
        pkgs.curl
      ];
    };

  testScript =
    { nodes, ... }:
    let
      specialisations = "${nodes.machine.system.build.toplevel}/specialisation";
      # PAPPL refuses requests for a bare host name other than its own.
      address = nodes.machine.networking.primaryIPAddress;
      getPrinterAttributes = "${nodes.client.nixpkgs.pkgs.cups.out}/share/cups/ipptool/get-printer-attributes.test";
    in
    ''
      SOCKET = "/run/phomemo-printer-app/phomemo-printer-app.sock"
      STATE_DIRECTORY = "/var/lib/phomemo-printer-app"
      STATE = f"{STATE_DIRECTORY}/phomemo-printer-app.state"


      def service_uid(node: BaseMachine = machine) -> int:
          pid = node.succeed(
              "systemctl show --property=MainPID --value phomemo-printer-app.service"
          ).strip()
          return int(node.succeed(f"ps -o euid= -p {pid}").strip())


      def assert_dynamic_user() -> None:
          uid = service_uid()
          assert 61184 <= uid <= 65519, f"the service runs as UID {uid}"
          machine.succeed("getent passwd phomemo-printer-app")
          machine.succeed(f"test -S {SOCKET} && test $(stat -c %u {SOCKET}) = {uid}")


      def listeners(port: int) -> set[str]:
          """The local addresses of the TCP sockets listening on `port`."""
          output = machine.succeed(f"ss -Hltn 'sport = :{port}'")
          return {line.split()[3] for line in output.splitlines()}


      def firewall_allows(port: int) -> bool:
          return machine.execute(f"iptables -S nixos-fw | grep -E -- '--dport {port}( |$)'")[0] == 0


      def cli(command: str, user: str | None = None) -> str:
          """Run a sub-command as root or as `user`, which must reach the service."""
          if user is None:
              output = machine.succeed(f"phomemo-printer-app {command}")
          else:
              output = machine.succeed(f"su -l {user} -c 'phomemo-printer-app {command}'")
          # Not a private server started for the sub-command.
          machine.fail("pgrep -f '[p]rivate-server=true'")
          machine.fail("find /tmp -maxdepth 1 -name 'phomemo-printer-app*.sock' | grep .")
          return output


      def switch(specialisation: str, port: int) -> None:
          machine.succeed(
              f"${specialisations}/{specialisation}/bin/switch-to-configuration test"
          )
          machine.wait_for_unit("phomemo-printer-app.service")
          machine.wait_for_open_port(port)


      def web_status(path: str, user: str | None = None) -> str:
          """The HTTP status of a web interface page, from the client."""
          login = f"-u {user}:{user}-password" if user else ""
          return client.succeed(
              f"curl -ks {login} -o /dev/null -w '%{{http_code}}' https://${address}:631{path}"
          )


      start_all()
      machine.wait_for_unit("phomemo-printer-app.service")
      machine.wait_for_open_port(8000)

      with subtest("the service runs as a dynamic user, configured by the module only"):
          assert_dynamic_user()
          machine.succeed('test -z "$(systemctl show -P EnvironmentFiles phomemo-printer-app)"')

      with subtest("the web interface answers on localhost only"):
          machine.succeed("curl -fsS http://localhost:8000/ | grep -F 'Phomemo Printer App'")
          assert listeners(8000) == {"127.0.0.1:8000", "[::1]:8000"}, listeners(8000)
          assert not firewall_allows(8000)
          client.fail("curl -fsS --max-time 5 http://${address}:8000/")

      with subtest("the sub-commands of every account reach the service"):
          cli("add -d test -v socket://localhost:9100 -m phomemo_m220", user="alice")
          assert cli("printers").split() == ["test"]
          assert cli("printers", user="alice").split() == ["test"]
          assert "phomemo_m220" in cli("drivers", user="alice")

      with subtest("BlueZ without an adapter answers with no printers"):
          machine.succeed("systemctl is-active bluetooth.service")
          assert cli("devices", user="alice").strip() == ""
          machine.fail("journalctl -u phomemo-printer-app | grep -F 'Unable to list Bluetooth'")

      with subtest("the printers persist across a restart"):
          machine.wait_until_succeeds(f"grep -F 'name=\"test\"' {STATE}")
          machine.succeed("systemctl restart phomemo-printer-app.service")
          machine.wait_for_open_port(8000)
          assert_dynamic_user()
          assert cli("printers").split() == ["test"]

      with subtest("the root variant starts, with the printers"):
          switch("root", 631)
          assert service_uid() == 0
          machine.succeed(f"test -S {SOCKET}")
          assert cli("printers", user="alice").split() == ["test"]

      with subtest("openFirewall opens the port to the network"):
          assert listeners(631) == {"0.0.0.0:631", "[::]:631"}, listeners(631)
          assert firewall_allows(631)

      with subtest("the root variant checks remote logins with PAM and the admin group"):
          assert web_status("/") == "200"
          assert web_status("/config") == "401"
          assert web_status("/config", user="alice") == "200"
          assert web_status("/config", user="bob") == "403"
          machine.succeed(
              "journalctl -u phomemo-printer-app"
              " | grep -F 'Authenticated as \"alice\" using Basic.'"
          )

      with subtest("tlsOnly advertises only ipps to the network"):
          client.succeed("curl -fsS http://${address}:631/ | grep -F 'Phomemo Printer App'")
          uris = client.succeed(
              "ipptool -tv ipp://${address}:631/ipp/print ${getPrinterAttributes}"
              " | grep -F 'printer-uri-supported ('"
          )
          assert "ipps://" in uris and "ipp://" not in uris, uris

      with subtest("the root variant keeps its TLS credentials from CUPS'"):
          machine.succeed(f'test -n "$(ls -A {STATE_DIRECTORY}/ssl)"')
          machine.succeed('test -z "$(ls -A /etc/cups)"')

      with subtest("without BlueZ, finding printers fails gracefully"):
          machine.fail("systemctl is-active bluetooth.service")
          assert cli("devices").strip() == ""
          machine.succeed("journalctl -u phomemo-printer-app | grep -F 'Unable to list Bluetooth'")
          machine.succeed("systemctl is-active phomemo-printer-app.service")

      with subtest("register-cups adds a CUPS queue for the root variant"):
          switch("cups", 8000)
          assert service_uid() == 0
          cli("register-cups --port 8000")
          machine.succeed("lpstat -v phomemo | grep -F 'ipp://localhost:8000/ipp/print'")

      with subtest("beside CUPS, the root variant keeps to its own TLS credentials"):
          cups_credentials = machine.succeed("ls -A /var/lib/cups/ssl")
          machine.succeed("curl -kfsS https://localhost:8000/ | grep -F 'Phomemo Printer App'")
          assert machine.succeed("ls -A /var/lib/cups/ssl") == cups_credentials

      with subtest("the firewall stays closed without openFirewall"):
          assert listeners(8000) == {"0.0.0.0:8000", "[::]:8000"}, listeners(8000)
          assert not firewall_allows(8000)
          client.fail("curl -fsS --max-time 5 http://${address}:8000/")

      with subtest("the dynamic user gets the state back, logs to a file, and binds a port below 1024"):
          switch("lowport", 631)
          assert_dynamic_user()
          assert cli("printers", user="alice").split() == ["test"]
          machine.succeed("curl -fsS http://localhost:631/ | grep -F 'Phomemo Printer App'")
          log_file = f"{STATE_DIRECTORY}/server.log"
          machine.succeed(f"grep -F \"Listening for connections on 'localhost:631'\" {log_file}")
          pid = machine.succeed("systemctl show -P MainPID phomemo-printer-app").strip()
          machine.fail(f"journalctl _PID={pid} | grep -F 'Listening for connections'")

      with subtest("the root variant starts with an immutable /etc"):
          immutable.wait_for_unit("phomemo-printer-app.service")
          immutable.wait_for_open_port(8000)
          assert service_uid(immutable) == 0
          immutable.fail("touch /etc/probe")
          immutable.succeed("curl -kfsS https://localhost:8000/ | grep -F 'Phomemo Printer App'")
          immutable.succeed(f'test -n "$(ls -A {STATE_DIRECTORY}/ssl)"')
          immutable.succeed('test -z "$(ls -A /etc/cups)"')
    '';
}
