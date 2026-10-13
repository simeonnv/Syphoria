{
  pkgs,
  config,
  ...
}: let
  state = "${config.devenv.state}/stalwart";
  password = "${state}/admin-password";
  secret = "%{file:${password}}%";

  stalwartConfig = (pkgs.formats.toml {}).generate "stalwart.toml" {
    server = {
      hostname = "localhost";
      listener = {
        http = {
          bind = ["127.0.0.1:8080"];
          protocol = "http";
        };
        smtp = {
          bind = ["127.0.0.1:2525"];
          protocol = "smtp";
        };
        imap = {
          bind = ["127.0.0.1:1143"];
          protocol = "imap";
        };
      };
    };

    storage = {
      data = "rocksdb";
      fts = "rocksdb";
      blob = "rocksdb";
      lookup = "rocksdb";
      directory = "internal";
    };
    store.rocksdb = {
      type = "rocksdb";
      path = "${state}/data";
      compression = "lz4";
    };
    directory.internal = {
      type = "internal";
      store = "rocksdb";
      cache.size = "disable";
    };

    tracer.stdout = {
      type = "stdout";
      level = "info";
      ansi = false;
      enable = true;
    };

    authentication = {
      fallback-admin = {
        user = "admin";
        inherit secret;
      };
      master = {
        user = "admin";
        inherit secret;
      };
    };

    session.auth.require = false;
    session.data.limits.size = 150000000;
    jmap = {
      protocol.upload = {
        max-size = 100000000;
        quota.size = 1000000000;
      };
      email = {
        max-attachment-size = 100000000;
        max-size = 150000000;
      };
    };
    spam-filter.enable = false;
    auth = {
      spf.verify = {
        ehlo = "disable";
        mail-from = "disable";
      };
      iprev.verify = "disable";
      dmarc.verify = "disable";
      arc.verify = "disable";
      dkim.verify = "disable";
    };

    config.local-keys = [
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
      "cluster.*"
      "storage.data"
      "storage.blob"
      "storage.lookup"
      "storage.fts"
      "storage.directory"
      "enterprise.license-key"
      "session.*"
      "spam-filter.*"
      "auth.*"
      "jmap.protocol.upload.max-size"
      "jmap.protocol.upload.quota.size"
      "jmap.email.max-attachment-size"
      "jmap.email.max-size"
    ];
  };
in {
  languages.rust = {
    enable = true;
    channel = "stable";
    version = "latest";
  };

  processes.stalwart.exec = ''
    mkdir -p ${state}
    [ -s ${password} ] || head -c 32 /dev/urandom | base64 | tr -dc 'A-Za-z0-9' > ${password}
    exec ${pkgs.stalwart}/bin/stalwart --config ${stalwartConfig}
  '';
}
