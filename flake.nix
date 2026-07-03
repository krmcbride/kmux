{
  description = "Lean tmux and git worktree workflow helper";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (
        _: pkgs: {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "kmux";
            version = self.shortRev or self.dirtyShortRev or "dev";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [ pkgs.installShellFiles ];
            nativeCheckInputs = [ pkgs.git ];
            postInstall = ''
              export HOME=$TMPDIR
              installShellCompletion --cmd kmux \
                --bash <($out/bin/kmux completions bash) \
                --fish <($out/bin/kmux completions fish) \
                --zsh <($out/bin/kmux completions zsh)

              install -Dm0644 integrations/opencode/README.md \
                $out/share/kmux/integrations/opencode/README.md
              install -Dm0644 integrations/opencode/kmux-status-server.ts \
                $out/share/kmux/integrations/opencode/kmux-status-server.ts
              install -Dm0644 integrations/opencode/kmux-status-tui.ts \
                $out/share/kmux/integrations/opencode/kmux-status-tui.ts
              install -Dm0755 integrations/opencode/kmux-select-session.ts \
                $out/share/kmux/integrations/opencode/kmux-select-session.ts
            '';
          };
        }
      );

      devShells = forAllSystems (
        _: pkgs: {
          default = pkgs.mkShell {
            packages = with pkgs; [
              bun
              cargo
              clippy
              git
              jq
              just
              rust-analyzer
              rustc
              rustfmt
              tmux
            ];
          };
        }
      );

      checks = forAllSystems (
        system: _: {
          kmux = self.packages.${system}.default;
        }
      );

      formatter = forAllSystems (_: pkgs: pkgs.nixpkgs-fmt);
    };
}
