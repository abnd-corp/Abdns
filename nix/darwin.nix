# Module nix-darwin : relais sur 127.0.0.1:53 et [::1]:53, service launchd en administrateur.
# La configuration reste un fichier modifiable (/etc/abdns/config.toml par défaut), relu à
# chaud par abdns : `initialSettings` ne sert qu'à le créer s'il n'existe pas.
# Les jetons ne sont pas gérés ici (ils finiraient lisibles par tous dans le magasin Nix) :
# `sudo abdns token` les range dans le token_file du serveur.
self: { config, lib, pkgs, ... }:
let
  cfg = config.services.abdns;
  package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
  logDir = "/var/log/abdns";
  initialFile = (pkgs.formats.toml { }).generate "abdns.toml" cfg.initialSettings;
in
{
  options.services.abdns = {
    enable = lib.mkEnableOption "abdns, encrypted DNS for this Mac";
    configFile = lib.mkOption {
      type = lib.types.str;
      default = "/etc/abdns/config.toml";
      description = "Editable configuration file, reloaded by abdns whenever it changes.";
    };
    initialSettings = lib.mkOption {
      type = lib.types.nullOr (pkgs.formats.toml { }).type;
      default = null;
      example = lib.literalExpression ''
        {
          language = "en";
          local_fallback = true;
          server = [
            { name = "My resolver"; url = "https://203.0.113.5/dns-query/"; token_file = "/etc/abdns/token"; }
            { name = "Mullvad"; url = "https://dns.mullvad.net/dns-query"; ip = "194.242.2.2"; }
          ];
        }
      '';
      description = "Written to configFile only when it does not exist yet; edit the file afterwards.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ package ];
    # Rotation à 1 Mio, 3 générations compressées.
    environment.etc."newsyslog.d/abdns.conf".text = ''
      ${logDir}/abdns.log 644 3 1024 * Z
    '';
    launchd.daemons.abdns.serviceConfig = {
      ProgramArguments = [ "${package}/bin/abdnsd" "--config" cfg.configFile ];
      RunAtLoad = true;
      KeepAlive = true;
      ThrottleInterval = 30;
      StandardOutPath = "${logDir}/abdns.log";
      StandardErrorPath = "${logDir}/abdns.log";
    };
    system.activationScripts.postActivation.text = ''
      mkdir -p ${logDir}
    '' + lib.optionalString (cfg.initialSettings != null) ''
      if [ ! -e ${lib.escapeShellArg cfg.configFile} ]; then
        install -d -m 755 "$(dirname ${lib.escapeShellArg cfg.configFile})"
        install -m 644 ${initialFile} ${lib.escapeShellArg cfg.configFile}
      fi
    '';
  };
}
