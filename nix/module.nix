{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.syphoria;
  toml = pkgs.formats.toml {};
  configFile = toml.generate "syphoria.toml" cfg.settings;

  stateDir = "/var/lib/syphoria";
  credential = "/run/credentials/syphoria.service/stalwart-admin";
  stalwartCredential = "/run/credentials/stalwart.service/syphoria-admin";
  stalwartSecret = "%{file:${stalwartCredential}}%";
  stalwartLocal = config.services.stalwart.enable;

  listenPort = lib.toInt (lib.last (lib.splitString ":" cfg.settings.server.listen));
  privilegedPort = listenPort < 1024;

  fileQuota = cfg.settings.files.quota_mb * 1000000;
  messageLimit = fileQuota * 3 / 2;

  services = ["syphoria.service"] ++ lib.optional stalwartLocal "stalwart.service";

  backupScript = pkgs.writeShellApplication {
    name = "syphoria-backup";
    runtimeInputs = with pkgs; [
      coreutils
      findutils
      gnutar
      zstd
      systemd
    ];
    text = ''
      dir=${lib.escapeShellArg cfg.backup.dir}
      stamp=$(date -u +%Y%m%dT%H%M%SZ)
      staging=$(mktemp -d "$dir/.staging.XXXXXX")
      stopped=1
      cleanup() {
        if [ "$stopped" = 1 ]; then systemctl start ${lib.concatStringsSep " " services}; fi
        rm -rf "$staging"
      }
      trap cleanup EXIT

      systemctl stop ${lib.concatStringsSep " " services}

      cp -a --reflink=auto ${stateDir}/. "$staging/syphoria"
      ${lib.optionalString stalwartLocal ''
        cp -a --reflink=auto ${config.services.stalwart.dataDir}/. "$staging/stalwart"
      ''}

      systemctl start ${lib.concatStringsSep " " services}
      stopped=0

      tar -C "$staging" --zstd -cf "$dir/.syphoria-$stamp.tar.zst" .
      mv "$dir/.syphoria-$stamp.tar.zst" "$dir/syphoria-$stamp.tar.zst"

      find "$dir" -maxdepth 1 -name 'syphoria-*.tar.zst' -printf '%f\n' \
        | sort -r \
        | tail -n +${toString (cfg.backup.keep + 1)} \
        | xargs -r -I{} rm -- "$dir/{}"
    '';
  };
in {
  options.services.syphoria = {
    enable = lib.mkEnableOption "syphoria, a email multiplexer and password vault served over SSH";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix {};
      defaultText = lib.literalExpression "pkgs.callPackage ./package.nix { }";
      description = "The syphoria package to use.";
    };

    openFirewall = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Open the SSH port from {option}`services.syphoria.settings.server.listen`.";
    };

    settings = lib.mkOption {
      description = ''
        Contents of `syphoria.toml`. See `syphoria.example.toml`.
        `server.host_key`, `vault.dir`, `files.dir` and `stalwart.admin_password_file` are managed by the module.
      '';
      default = {};
      type = lib.types.submodule {
        freeformType = toml.type;
        options = {
          server.listen = lib.mkOption {
            type = lib.types.str;
            default = "0.0.0.0:2222";
            description = "Address the SSH server listens on.";
          };
          stalwart.url = lib.mkOption {
            type = lib.types.str;
            description = "Base URL of the Stalwart HTTP listener (management API and JMAP).";
          };
          stalwart.admin_user = lib.mkOption {
            type = lib.types.str;
            default = "admin";
            description = "Stalwart admin and master user.";
          };
          files.quota_mb = lib.mkOption {
            type = lib.types.ints.positive;
            default = 100;
            description = "Per-user file storage (SFTP uploads, saved attachments) in MB.";
          };
        };
      };
    };

    stalwart = {
      setup = lib.mkEnableOption ''
        a local Stalwart configured for syphoria: an HTTP listener on localhost,
        admin and master users from {option}`services.syphoria.stalwart.passwordFile`,
        and disabled directory caching. {option}`services.stalwart.stateVersion` must still be set
      '';

      passwordFile = lib.mkOption {
        type = lib.types.path;
        description = "File containing the Stalwart admin password. Not copied to the store.";
      };

      port = lib.mkOption {
        type = lib.types.port;
        default = 8080;
        description = "Port of the localhost HTTP listener added to Stalwart when `setup` is enabled.";
      };
    };

    backup = {
      enable = lib.mkEnableOption ''
        periodic backups of the syphoria state (vaults, host key) and, if enabled on this host,
        the Stalwart data directory. Both services are stopped while the files are copied
      '';

      dir = lib.mkOption {
        type = lib.types.path;
        default = "/var/backup/syphoria";
        description = "Directory receiving `syphoria-<timestamp>.tar.zst` archives.";
      };

      startAt = lib.mkOption {
        type = with lib.types; either str (listOf str);
        default = "daily";
        description = "When to run the backup, in {manpage}`systemd.time(7)` format.";
      };

      keep = lib.mkOption {
        type = lib.types.ints.positive;
        default = 7;
        description = "Number of archives to keep.";
      };
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        services.syphoria.settings = {
          server.host_key = "${stateDir}/host_ed25519";
          vault.dir = "${stateDir}/vault";
          files.dir = "${stateDir}/files";
          stalwart.admin_password_file = credential;
        };

        networking.firewall.allowedTCPPorts = lib.mkIf cfg.openFirewall [listenPort];

        systemd.services.syphoria = {
          description = "syphoria SSH mail server";
          wantedBy = ["multi-user.target"];
          wants = ["network-online.target"] ++ lib.optional stalwartLocal "stalwart.service";
          after = ["network-online.target"] ++ lib.optional stalwartLocal "stalwart.service";

          serviceConfig = {
            ExecStart = "${lib.getExe cfg.package} --config ${configFile}";
            Restart = "on-failure";
            RestartSec = 5;

            DynamicUser = true;
            StateDirectory = "syphoria";
            StateDirectoryMode = "0700";
            LoadCredential = ["stalwart-admin:${cfg.stalwart.passwordFile}"];

            AmbientCapabilities = lib.optional privilegedPort "CAP_NET_BIND_SERVICE";
            CapabilityBoundingSet =
              if privilegedPort
              then ["CAP_NET_BIND_SERVICE"]
              else [""];
            DeviceAllow = [""];
            LockPersonality = true;
            MemoryDenyWriteExecute = true;
            NoNewPrivileges = true;
            PrivateDevices = true;
            PrivateTmp = true;
            PrivateUsers = !privilegedPort;
            ProcSubset = "pid";
            ProtectClock = true;
            ProtectControlGroups = true;
            ProtectHome = true;
            ProtectHostname = true;
            ProtectKernelLogs = true;
            ProtectKernelModules = true;
            ProtectKernelTunables = true;
            ProtectProc = "invisible";
            ProtectSystem = "strict";
            RestrictAddressFamilies = [
              "AF_INET"
              "AF_INET6"
              "AF_UNIX"
            ];
            RestrictNamespaces = true;
            RestrictRealtime = true;
            RestrictSUIDSGID = true;
            SystemCallArchitectures = "native";
            SystemCallFilter = [
              "@system-service"
              "~@privileged"
              "~@resources"
            ];
            UMask = "0077";
          };
        };
      }

      (lib.mkIf cfg.stalwart.setup {
        services.syphoria.settings.stalwart.url = lib.mkDefault "http://127.0.0.1:${toString cfg.stalwart.port}";

        services.stalwart = {
          enable = true;
          credentials.syphoria-admin = toString cfg.stalwart.passwordFile;
          settings = {
            server.listener.syphoria = {
              bind = ["127.0.0.1:${toString cfg.stalwart.port}"];
              protocol = "http";
            };
            http.url = lib.mkDefault [
              {
                "if" = "listener == 'syphoria'";
                "then" = "'http://127.0.0.1:${toString cfg.stalwart.port}'";
              }
              {"else" = "protocol + '://' + config_get('server.hostname') + ':' + local_port";}
            ];
            authentication.fallback-admin = {
              user = cfg.settings.stalwart.admin_user;
              secret = stalwartSecret;
            };
            authentication.master = {
              user = cfg.settings.stalwart.admin_user;
              secret = stalwartSecret;
            };
            directory.internal.cache.size = "disable";
            jmap.protocol.upload.max-size = fileQuota;
            jmap.protocol.upload.quota.size = fileQuota * 10;
            jmap.email.max-attachment-size = fileQuota;
            jmap.email.max-size = messageLimit;
            session.data.limits.size = messageLimit;
            config.local-keys = lib.mkDefault [
              "jmap.protocol.upload.max-size"
              "jmap.protocol.upload.quota.size"
              "jmap.email.max-attachment-size"
              "jmap.email.max-size"
              "session.data.limits.size"
              "store.*"
              "directory.*"
              "tracer.*"
              "!server.blocked-ip.*"
              "!server.allowed-ip.*"
              "server.*"
              "certificate.*"
              "config.local-keys.*"
              "authentication.fallback-admin.*"
              "authentication.master.*"
              "http.url"
              "http.url.*"
              "cluster.*"
              "storage.data"
              "storage.blob"
              "storage.lookup"
              "storage.fts"
              "storage.directory"
              "enterprise.license-key"
            ];
          };
        };
      })

      (lib.mkIf cfg.backup.enable {
        systemd.tmpfiles.rules = ["d '${cfg.backup.dir}' 0700 root root - -"];

        systemd.services.syphoria-backup = {
          description = "syphoria backup";
          inherit (cfg.backup) startAt;
          serviceConfig = {
            Type = "oneshot";
            ExecStart = lib.getExe backupScript;
            UMask = "0077";
          };
        };
      })
    ]
  );
}
