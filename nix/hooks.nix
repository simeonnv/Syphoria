{lib, ...}: {
  git-hooks.hooks = {
    rustfmt.enable = true;
    clippy.enable = true;
  };

  tasks."devenv:git-hooks:run".before = lib.mkForce [];
}
