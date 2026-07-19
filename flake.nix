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
            nativeBuildInputs = [
              pkgs.installShellFiles
              pkgs.makeWrapper
            ];
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
              install -Dm0644 integrations/opencode/kmux-server-reporter.ts \
                $out/share/kmux/integrations/opencode/kmux-server-reporter.ts
              install -Dm0644 integrations/opencode/kmux-command-queue.ts \
                $out/share/kmux/integrations/opencode/kmux-command-queue.ts
              install -Dm0644 integrations/opencode/kmux-child-process.ts \
                $out/share/kmux/integrations/opencode/kmux-child-process.ts
              install -Dm0644 integrations/opencode/kmux-opencode-launcher.ts \
                $out/share/kmux/integrations/opencode/kmux-opencode-launcher.ts

              makeWrapper ${pkgs.bun}/bin/bun $out/bin/kmux-opencode-launcher \
                --add-flag --no-install \
                --add-flag --no-env-file \
                --add-flag $out/share/kmux/integrations/opencode/kmux-opencode-launcher.ts

              install -Dm0644 skills/delegating-with-kmux/SKILL.md \
                $out/share/kmux/skills/delegating-with-kmux/SKILL.md
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
