# Module nix-darwin : relais sur 127.0.0.1:53 et [::1]:53, service launchd en administrateur.
# Le jeton n'est pas géré ici (il finirait lisible par tous dans le magasin Nix) :
# `sudo abdns token` le range dans upstream.tokenFile.
self: { config, lib, pkgs, ... }:
let
  cfg = config.services.abdns;
  package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
  logDir = "/var/log/abdns";
  server = lib.types.submodule {
    options = {
      name = lib.mkOption { type = lib.types.str; };
      url = lib.mkOption { type = lib.types.str; };
      ip = lib.mkOption { type = lib.types.str; description = "Fixed address: the server is reached without any plaintext lookup."; };
    };
  };
  configFile = (pkgs.formats.toml { }).generate "abdns.toml" ({
    inherit (cfg) language fallback;
    dnsproxy = lib.getExe pkgs.dnsproxy;
    network_services = cfg.networkServices;
    upstream = { inherit (cfg.upstream) url; }
      // lib.optionalAttrs (cfg.upstream.ip != null) { inherit (cfg.upstream) ip; }
      // lib.optionalAttrs (cfg.upstream.tokenFile != null) { token_file = cfg.upstream.tokenFile; };
  });
in
{
  options.services.abdns = {
    enable = lib.mkEnableOption "abdns, encrypted DNS for this Mac";
    language = lib.mkOption {
      type = lib.types.enum [ "en" "fr" ];
      default = "en";
      description = "Language of notifications.";
    };
    upstream = {
      url = lib.mkOption {
        type = lib.types.str;
        example = "https://203.0.113.5/dns-query";
        description = "DoH URL of your own resolver; with tokenFile, the prefix the token is appended to (ending with /).";
      };
      ip = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Address of the resolver, required when url uses a host name.";
      };
      tokenFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "/etc/abdns/token";
        description = "Root-only file holding the secret path segment of the DoH URL.";
      };
    };
    fallback = lib.mkOption {
      type = lib.types.listOf server;
      default = [
        { name = "Mullvad"; url = "https://dns.mullvad.net/dns-query"; ip = "194.242.2.2"; }
        { name = "DNS.SB"; url = "https://doh.dns.sb/dns-query"; ip = "185.222.222.222"; }
        { name = "Digitale Gesellschaft"; url = "https://dns.digitale-gesellschaft.ch/dns-query"; ip = "185.95.218.42"; }
      ];
      description = "Encrypted fallbacks, used only when the upstream does not answer.";
    };
    networkServices = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ "Wi-Fi" ];
      description = "Network services (networksetup -listallnetworkservices) whose DNS goes through abdns; not VPNs.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ package ];
    environment.etc."abdns/config.toml".source = configFile;
    # Rotation à 1 Mio, 3 générations compressées.
    environment.etc."newsyslog.d/abdns.conf".text = ''
      ${logDir}/abdns.log 644 3 1024 * Z
    '';
    launchd.daemons.abdns.serviceConfig = {
      ProgramArguments = [ (lib.getExe package) "service" ];
      RunAtLoad = true;
      KeepAlive = true;
      ThrottleInterval = 30;
      StandardOutPath = "${logDir}/abdns.log";
      StandardErrorPath = "${logDir}/abdns.log";
    };
    system.activationScripts.postActivation.text = ''
      mkdir -p ${logDir}
    '';
  };
}
